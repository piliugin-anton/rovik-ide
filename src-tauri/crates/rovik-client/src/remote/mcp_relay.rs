//! MCP servers on this computer's loopback, for the runs and Tests on its
//! hosts (mcp-plan-2.md, Q11). A host can't reach `localhost` here, so its
//! POSTs to such a server come as `McpRelayWanted`: this computer takes
//! the POST (`McpRelayTake`, which one window does when several are open),
//! makes it when it's to one of its own MCP servers on the loopback, and
//! sends the answer back as it comes (`McpRelayPart`). A server on the old
//! HTTP+SSE transport (Q13) has its event stream's GET relayed so, and its
//! messages POSTed to the endpoint that stream named, while it lasts. No
//! other address here is the host's to reach.

use super::Remote;
use futures_util::StreamExt;
use rovik_core::mcp::{on_loopback, EndpointWatch, McpTransport};
use rovik_protocol::mcp::{RelayPart, RelayedPost};
use rovik_protocol::{Request, Response};

/// The most text one part carries: a frame holds 1 MB, and JSON may escape
/// a character as six.
const PART_TEXT: usize = 128 << 10;

/// Why a POST to another address isn't made.
const REFUSED: &str = "Rovik on the user's computer posts only to its own MCP servers on the \
                       loopback, as Settings → MCP servers has them.";

/// A URL as the MCP client writes it, for comparing two.
fn normal(url: &str) -> Option<String> {
    reqwest::Url::parse(url.trim()).ok().map(|u| u.to_string())
}

/// While it lives, `url` is posted to for a host, as its own servers are:
/// one under Test on a host, saved or not, or the endpoint an event stream
/// it relays named.
pub(super) struct Relayable<'a> {
    remote: &'a Remote,
    url: Option<String>,
}

impl Drop for Relayable<'_> {
    fn drop(&mut self) {
        let Some(url) = &self.url else { return };
        let mut state = self.remote.lock();
        if let Some(at) = state.mcp_relayable.iter().position(|u| u == url) {
            state.mcp_relayable.swap_remove(at);
        }
    }
}

impl Remote {
    /// `McpRelayWanted` from `host`: POST `id` is made on a task of its own.
    pub(super) fn mcp_relay_wanted(&self, host: &str, id: String) {
        let Some(me) = self.me.upgrade() else { return };
        let host = host.to_string();
        tokio::spawn(async move { me.relay(&host, id).await });
    }

    /// `transport`'s URL is under Test on a host until the guard goes.
    pub(super) fn testing(&self, transport: &McpTransport) -> Relayable<'_> {
        match transport {
            McpTransport::Url { url, .. } => self.relayable(url),
            McpTransport::Command { .. } => Relayable {
                remote: self,
                url: None,
            },
        }
    }

    /// `url` is posted to for a host until the guard goes.
    fn relayable(&self, url: &str) -> Relayable<'_> {
        let url = normal(url);
        if let Some(url) = &url {
            self.lock().mcp_relayable.push(url.clone());
        }
        Relayable { remote: self, url }
    }

    /// Whether `url` is one of this computer's MCP servers on the loopback:
    /// on in the settings, under Test on a host now, or the endpoint one's
    /// relayed event stream named.
    fn relays_to(&self, url: &str) -> bool {
        let Some(url) = normal(url).filter(|u| on_loopback(u)) else {
            return false;
        };
        if self.lock().mcp_relayable.contains(&url) {
            return true;
        }
        let servers = self.core.agents.config().mcp_servers;
        servers
            .iter()
            .filter(|s| s.enabled)
            .any(|s| match &s.transport {
                McpTransport::Url { url: theirs, .. } => normal(theirs).as_ref() == Some(&url),
                McpTransport::Command { .. } => false,
            })
    }

    /// Takes POST `id` from `host` and makes it, its answer going back part
    /// by part; one another window took, or the host gave up, is left.
    async fn relay(&self, host: &str, id: String) {
        let take = Request::McpRelayTake { id: id.clone() };
        let Ok(Response::McpRelayed(post)) = self.host_call(host, take).await else {
            return;
        };
        let send = |part: RelayPart| {
            let request = Request::McpRelayPart {
                id: id.clone(),
                part,
            };
            async move {
                let wanted = self.host_call(host, request).await;
                matches!(wanted, Ok(Response::Bool(true)))
            }
        };
        if !self.relays_to(&post.url) {
            send(RelayPart::Failed {
                error: REFUSED.into(),
            })
            .await;
            return;
        }
        let response = match posted(&post).await {
            Ok(response) => response,
            Err(error) => {
                send(RelayPart::Failed { error }).await;
                return;
            }
        };
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.to_string())))
            .collect();
        let head = RelayPart::Head {
            status: response.status().as_u16(),
            headers,
        };
        if !send(head).await {
            return;
        }
        // An event stream's endpoint is posted to while the stream lasts,
        // from before the host reads where it is.
        let mut watch = post.stream.then(|| EndpointWatch::new(&post.url)).flatten();
        let mut _endpoint = None;
        let mut body = response.bytes_stream();
        let mut pending: Vec<u8> = Vec::new();
        while let Some(chunk) = body.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(e) => {
                    let error = rovik_core::net::error_text(&e.without_url());
                    send(RelayPart::Failed { error }).await;
                    return;
                }
            };
            if let Some(endpoint) = watch.as_mut().and_then(|w| w.feed(&chunk)) {
                _endpoint = Some(self.relayable(&endpoint));
            }
            pending.extend_from_slice(&chunk);
            for text in texts(&mut pending, false) {
                if !send(RelayPart::Body { text }).await {
                    return;
                }
            }
        }
        for text in texts(&mut pending, true) {
            if !send(RelayPart::Body { text }).await {
                return;
            }
        }
        send(RelayPart::End).await;
    }
}

/// Makes `post` here, as the host had it: a POST, or the GET of an event
/// stream.
async fn posted(post: &RelayedPost) -> Result<reqwest::Response, String> {
    let client = rovik_core::net::client().await;
    let mut request = match post.stream {
        true => client.get(&post.url),
        false => client.post(&post.url).body(post.body.clone()),
    };
    for (name, value) in &post.headers {
        request = request.header(name.as_str(), value.0.as_str());
    }
    let sent = request.send().await;
    sent.map_err(|e| rovik_core::net::error_text(&e.without_url()))
}

/// The text `pending` holds, in parts of [`PART_TEXT`] at most, taken out
/// of it; a character the chunks cut is kept for the next, unless `last`.
fn texts(pending: &mut Vec<u8>, last: bool) -> Vec<String> {
    let whole = match std::str::from_utf8(pending) {
        Err(e) if e.error_len().is_none() && !last => e.valid_up_to(),
        _ => pending.len(),
    };
    let rest = pending.split_off(whole);
    let text = String::from_utf8_lossy(pending).into_owned();
    *pending = rest;
    let mut parts = Vec::new();
    let mut from = 0;
    while from < text.len() {
        let mut to = (from + PART_TEXT).min(text.len());
        while !text.is_char_boundary(to) {
            to -= 1;
        }
        parts.push(text[from..to].to_string());
        from = to;
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A character a chunk cut waits for the rest of it, and a long text
    /// goes in parts a frame holds.
    #[test]
    fn texts_keep_a_cut_character_and_fit_a_frame() {
        let mut pending = "aé".as_bytes()[..2].to_vec();
        assert_eq!(texts(&mut pending, false), ["a"]);
        assert_eq!(pending, [0xc3]);
        pending.push(0xa9);
        assert_eq!(texts(&mut pending, false), ["é"]);
        assert!(pending.is_empty());
        let mut long = "x".repeat(PART_TEXT + 5).into_bytes();
        let parts = texts(&mut long, false);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].len(), PART_TEXT);
        let mut cut = vec![b'a', 0xc3];
        assert_eq!(
            texts(&mut cut, true),
            ["a\u{fffd}"],
            "the last part takes all"
        );
    }

    /// Q11: this computer posts for a host only to its own MCP servers on
    /// the loopback: one on in the settings, or under Test on a host now,
    /// saved or not; never one that's off, another address here, or one
    /// elsewhere.
    #[tokio::test]
    async fn a_host_is_posted_for_only_to_this_computer_s_own_servers() {
        use rovik_protocol::mcp::McpServer;
        let test = rovik_core::testing::TestCore::new();
        let remote = Remote::new(test.core.clone());
        let server = |id: &str, url: &str, enabled| McpServer {
            id: id.into(),
            name: id.into(),
            enabled,
            transport: McpTransport::Url {
                url: url.into(),
                headers: Vec::new(),
                sse: false,
                oauth: None,
            },
            disabled_tools: Vec::new(),
        };
        let config = rovik_core::agent_run::AgentConfig {
            mcp_servers: vec![
                server("on", "http://localhost:3000/mcp", true),
                server("off", "http://localhost:3001/mcp", false),
                server("far", "https://mcp.example.com/mcp", true),
            ],
            ..Default::default()
        };
        let configure = Request::AgentConfigure(Box::new(config));
        rovik_core::dispatch(&test.core, configure).await.unwrap();
        assert!(remote.relays_to("http://localhost:3000/mcp"));
        assert!(!remote.relays_to("http://localhost:3001/mcp"), "off");
        assert!(!remote.relays_to("http://localhost:22/"), "another address");
        assert!(!remote.relays_to("https://mcp.example.com/mcp"), "not here");
        let new = server("new", "http://127.0.0.1:4000/mcp", false);
        let testing = remote.testing(&new.transport);
        assert!(remote.relays_to("http://127.0.0.1:4000/mcp"), "under Test");
        drop(testing);
        assert!(!remote.relays_to("http://127.0.0.1:4000/mcp"));
        // Q13: where a relayed event stream said to post, while it lasts.
        let endpoint = remote.relayable("http://localhost:3000/messages?sessionId=1");
        assert!(remote.relays_to("http://localhost:3000/messages?sessionId=1"));
        assert!(!remote.relays_to("http://localhost:3000/messages?sessionId=2"));
        drop(endpoint);
        assert!(!remote.relays_to("http://localhost:3000/messages?sessionId=1"));
    }

    /// Q13: a relayed event stream is a GET, with its headers and no body;
    /// a message is a POST of its body.
    #[tokio::test]
    async fn an_event_stream_is_got_and_a_message_posted() {
        use rovik_protocol::Secret;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let url = format!("http://{}/sse", listener.local_addr().unwrap());
        let heard = tokio::spawn(async move {
            let mut heard = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = vec![0; 8192];
                let n = stream.read(&mut buf).await.unwrap();
                heard.push(String::from_utf8_lossy(&buf[..n]).into_owned());
                let answer = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                stream.write_all(answer).await.unwrap();
            }
            heard
        });
        let request = |stream| RelayedPost {
            url: url.clone(),
            headers: vec![("Authorization".into(), Secret("Bearer t0p".into()))],
            body: "{\"id\":1}".into(),
            stream,
        };
        posted(&request(true)).await.unwrap();
        posted(&request(false)).await.unwrap();
        let heard = heard.await.unwrap();
        assert!(heard[0].starts_with("GET /sse "), "{}", heard[0]);
        assert!(heard[0]
            .to_lowercase()
            .contains("authorization: bearer t0p"));
        assert!(!heard[0].contains("{\"id\":1}"), "no body");
        assert!(heard[1].starts_with("POST /sse "), "{}", heard[1]);
        assert!(heard[1].ends_with("{\"id\":1}"), "{}", heard[1]);
    }

    /// Two URLs compare as the MCP client writes them.
    #[test]
    fn a_url_compares_as_the_client_writes_it() {
        assert_eq!(
            normal(" http://localhost:3000/mcp "),
            normal("http://localhost:3000/mcp")
        );
        assert_ne!(
            normal("http://localhost:3000/mcp"),
            normal("http://localhost:3001/mcp")
        );
    }
}
