//! The port a browser tab of the app shows a forwarded page through
//! (remote/pages.rs). The tab is an iframe, and a page whose server says
//! it mustn't be framed (`X-Frame-Options`, or CSP's `frame-ancestors`,
//! as Django, Rails, Spring and helmet do by default) would show nothing
//! there. So this port speaks HTTP, where the page's own forwarded port
//! carries bytes: each request goes to the same server on the host, and
//! each response comes back as the server sent it, but for those headers.
//! On Windows its cookies are made ones a frame keeps (`frame_cookie`).
//! WebSocket upgrades (a dev server's hot reload) go through untouched.
//! And each page the tab loads gets the tag of a script of Rovik's after it
//! (`TAB_SCRIPT`, tab.js), which tells the tab where the page is as it
//! navigates. The page's own port stays as it was, for the default
//! browser.

use crate::connector::Stream;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::client::conn::http1::SendRequest;
use hyper::header::{
    HeaderMap, HeaderValue, ACCEPT, ACCEPT_ENCODING, CACHE_CONTROL, CONTENT_DISPOSITION,
    CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_SECURITY_POLICY, CONTENT_TYPE, SET_COOKIE, UPGRADE,
    X_FRAME_OPTIONS,
};
use hyper::upgrade::OnUpgrade;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{ready, Context, Poll};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

/// Whether the tab's cookies are made ones a frame keeps (`frame_cookie`).
/// Windows' WebView2 takes the app (`http://tauri.localhost`) and the page
/// (`http://localhost:<port>`) for two sites, and keeps no cookie in a
/// frame of another site but a `SameSite=None` one: without this, nothing
/// signs in there. WebKit (Linux, macOS) takes the page's `localhost` for
/// the app's own (`tauri://localhost`): it keeps the server's cookies as
/// they are, and would drop the rewritten ones on plain http.
pub(super) const FRAME_COOKIES: bool = cfg!(windows);

type Body = BoxBody<Bytes, hyper::Error>;

/// Where a page in a tab gets Rovik's script (tab.js), which tells the tab
/// where the page is as it navigates (src/components/BrowserView.tsx).
/// This port answers it: the page's server never sees the path.
const TAB_SCRIPT: &str = "/__rovik/tab.js";

/// Carries `local`, a connection of a browser tab's, to the page's server
/// on its host: its requests, one at a time, over a stream `connect`
/// opens, and a new one once the server closed the last. With
/// `frame_cookies`, the responses' cookies are made ones a frame keeps.
pub(super) async fn carry<C, F>(local: TcpStream, connect: C, frame_cookies: bool)
where
    C: Fn() -> F + Send + Sync + 'static,
    F: Future<Output = Result<Stream, String>> + Send,
{
    let upstream = Arc::new(Upstream {
        connect,
        sender: Mutex::new(None),
    });
    let service = hyper::service::service_fn(move |request| {
        let upstream = upstream.clone();
        async move { upstream.relay(request, frame_cookies).await }
    });
    let served = hyper::server::conn::http1::Builder::new()
        .timer(TokioTimer::new())
        .serve_connection(TokioIo::new(local), service)
        .with_upgrades()
        .await;
    if let Err(e) = served {
        log::debug!("a browser tab's connection: {e}");
    }
}

/// The server a tab's connection reaches: how to open a stream to it, and
/// the one its requests (of body `B`, the tab's) go over now, once there
/// is one.
struct Upstream<C, B> {
    connect: C,
    sender: Mutex<Option<SendRequest<B>>>,
}

impl<C, F, B> Upstream<C, B>
where
    C: Fn() -> F,
    F: Future<Output = Result<Stream, String>>,
    B: hyper::body::Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    /// The server's answer to `request`, framable (`frameable`); "502 Bad
    /// Gateway" with why when no stream to the server can be had. An
    /// upgrade (a WebSocket) the server takes joins the tab's connection to
    /// its stream. A page the tab loads comes with Rovik's script's tag
    /// after it; the script is this port's own. An error closes the tab's
    /// connection unanswered.
    async fn relay(
        &self,
        mut request: Request<B>,
        frame_cookies: bool,
    ) -> Result<Response<Body>, Dropped> {
        if request.uri().path() == TAB_SCRIPT {
            return Ok(tab_script());
        }
        // Asked for as it is, not compressed, for the tag to go after it.
        let page = loads_page(&request);
        if page {
            let identity = HeaderValue::from_static("identity");
            request.headers_mut().insert(ACCEPT_ENCODING, identity);
        }
        let tab_side = request
            .headers()
            .contains_key(UPGRADE)
            .then(|| hyper::upgrade::on(&mut request));
        let mut response = match self.send(request).await {
            Ok(response) => response,
            Err(Unanswered::Unreachable(why)) => return Ok(bad_gateway(why)),
            Err(Unanswered::Dropped) => return Err(Dropped),
        };
        if response.status() == StatusCode::SWITCHING_PROTOCOLS {
            // The stream is the upgrade's now: a next request needs another.
            self.sender.lock().await.take();
            if let Some(tab_side) = tab_side {
                let server_side = hyper::upgrade::on(&mut response);
                tokio::spawn(join(tab_side, server_side));
            }
        }
        frameable(response.headers_mut());
        if frame_cookies {
            frame_cookies_in(response.headers_mut());
        }
        if page && takes_script(&response) {
            return Ok(with_script(response));
        }
        Ok(response.map(BodyExt::boxed))
    }

    /// Sends `request` to the server, over the stream kept from the last
    /// one, else a new one. A server closes a kept stream it let go idle
    /// as it likes, maybe just as the request goes: unsent (the stream was
    /// seen closed), the request goes over a new stream then; sent, it's
    /// `Unanswered::Dropped`, as the server's own connection would be.
    async fn send(&self, mut request: Request<B>) -> Result<Response<Incoming>, Unanswered> {
        let mut sender = self.sender.lock().await;
        let mut reused = sender.is_some();
        loop {
            let open = match sender.take() {
                Some(open) => open,
                None => self.open().await.map_err(Unanswered::Unreachable)?,
            };
            match sender.insert(open).try_send_request(request).await {
                Ok(response) => return Ok(response),
                Err(mut failed) => {
                    // The stream failed: none is kept.
                    sender.take();
                    match failed.take_message() {
                        Some(unsent) if reused => {
                            request = unsent;
                            reused = false;
                        }
                        _ if reused => return Err(Unanswered::Dropped),
                        _ => {
                            return Err(Unanswered::Unreachable(format!(
                                "its server didn't answer ({})",
                                failed.error()
                            )))
                        }
                    }
                }
            }
        }
    }

    /// A new stream to the server, ready for requests.
    async fn open(&self) -> Result<SendRequest<B>, String> {
        let stream = (self.connect)().await?;
        let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| format!("its server didn't take the connection ({e})"))?;
        // Ends once the sender goes: with the tab's connection.
        tokio::spawn(async move {
            if let Err(e) = connection.with_upgrades().await {
                log::debug!("a browser tab's stream to its page's server: {e}");
            }
        });
        Ok(sender)
    }
}

/// Why the server's answer to a request didn't come.
#[derive(Debug)]
enum Unanswered {
    /// No stream to it could be had, or a new one took no request: why,
    /// for the tab to show.
    Unreachable(String),
    /// A kept stream the server closed after the request went. The tab's
    /// connection closes unanswered, as the server's would: a browser
    /// sends a request again over a new connection then, when it went over
    /// one it had used before.
    Dropped,
}

/// The error that closes a tab's connection unanswered.
#[derive(Debug)]
struct Dropped;

impl std::fmt::Display for Dropped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the page's server closed its connection before it answered")
    }
}

impl std::error::Error for Dropped {}

/// Joins an upgraded connection of the tab's to the server's, until either
/// side ends.
async fn join(tab_side: OnUpgrade, server_side: OnUpgrade) {
    let (Ok(tab_side), Ok(server_side)) = tokio::join!(tab_side, server_side) else {
        return;
    };
    let (mut tab_side, mut server_side) = (TokioIo::new(tab_side), TokioIo::new(server_side));
    let _ = tokio::io::copy_bidirectional(&mut tab_side, &mut server_side).await;
}

/// What the tab shows when the page's server can't be reached.
fn bad_gateway(why: String) -> Response<Body> {
    let mut response = whole(format!("Rovik couldn't show this page: {why}"));
    *response.status_mut() = StatusCode::BAD_GATEWAY;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

/// A response of this port's own, with `body`.
fn whole(body: impl Into<Bytes>) -> Response<Body> {
    let body = Full::new(body.into());
    Response::new(body.map_err(|never| match never {}).boxed())
}

/// Rovik's script for a page in a tab (`TAB_SCRIPT`).
fn tab_script() -> Response<Body> {
    let mut response = whole(include_str!("tab.js"));
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/javascript; charset=utf-8"),
    );
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

/// Whether `request` loads a page into the tab: a GET for a document or a
/// frame, as the browser says (`Sec-Fetch-Dest`), or, where it doesn't
/// say, one that takes HTML first. Not a script's fetch: a script may run
/// the HTML it fetches (htmx).
fn loads_page<B>(request: &Request<B>) -> bool {
    let header = |name| request.headers().get(name).and_then(|v| v.to_str().ok());
    if request.method() != Method::GET {
        return false;
    }
    match header("sec-fetch-dest") {
        Some(dest) => ["document", "iframe"]
            .iter()
            .any(|page| dest.trim().eq_ignore_ascii_case(page)),
        None => header(ACCEPT.as_str()).is_some_and(|accept| {
            let first = accept.split(',').next().unwrap_or_default();
            first.trim().eq_ignore_ascii_case("text/html")
        }),
    }
}

/// Whether `response`, to a page load, is HTML the script's tag can follow
/// as it is: not one without a body, a compressed one (the request asked
/// for none, but a server may compress all the same), a download, or one
/// in UTF-16, where the tag's bytes would be garbage.
fn takes_script(response: &Response<Incoming>) -> bool {
    let status = response.status();
    let bodiless = status.is_informational()
        || [
            StatusCode::NO_CONTENT,
            StatusCode::RESET_CONTENT,
            StatusCode::PARTIAL_CONTENT,
            StatusCode::NOT_MODIFIED,
        ]
        .contains(&status);
    let headers = response.headers();
    let text = |name| {
        let value = headers.get(name).and_then(|v| v.to_str().ok());
        value.unwrap_or_default().trim().to_ascii_lowercase()
    };
    let kind = text(CONTENT_TYPE);
    let html = kind.split(';').next().unwrap_or_default().trim() == "text/html";
    let encoding = text(CONTENT_ENCODING);
    let plain = encoding.is_empty() || encoding == "identity";
    let download = text(CONTENT_DISPOSITION).starts_with("attachment");
    !bodiless && html && !kind.contains("utf-16") && plain && !download
}

/// `response`, a page, with the tag of Rovik's script after it, carrying
/// the nonce its CSP lets scripts run with, if it asks for one. Longer by
/// the tag, it goes without its length (chunked).
fn with_script(mut response: Response<Incoming>) -> Response<Body> {
    let nonce = script_nonce(response.headers())
        .map(|nonce| format!(" nonce=\"{nonce}\""))
        .unwrap_or_default();
    let tag = Bytes::from(format!("<script src=\"{TAB_SCRIPT}\"{nonce}></script>"));
    response.headers_mut().remove(CONTENT_LENGTH);
    response.map(|page| {
        let body = WithScript {
            page,
            tag: Some(tag),
            trailers: None,
        };
        body.boxed()
    })
}

/// The nonce the response's CSP lets a `<script src>` run with, if it asks
/// for one: the first `'nonce-…'` of the directive of a policy that
/// governs it, `script-src-elem`, else `script-src`, else `default-src`.
/// A policy that lets the page's own scripts run (`'self'`) lets the tag's
/// run with no nonce.
fn script_nonce(headers: &HeaderMap) -> Option<String> {
    let mut policies = headers
        .get_all(CONTENT_SECURITY_POLICY)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|text| text.split(','));
    policies.find_map(|policy| {
        let directives: Vec<Vec<&str>> = policy
            .split(';')
            .map(|directive| directive.split_ascii_whitespace().collect())
            .collect();
        let named = |name: &str| {
            directives
                .iter()
                .find(|d| d.first().is_some_and(|n| n.eq_ignore_ascii_case(name)))
        };
        let governing = ["script-src-elem", "script-src", "default-src"]
            .into_iter()
            .find_map(named)?;
        governing.iter().skip(1).find_map(|source| nonce_in(source))
    })
}

/// The value of a CSP source `'nonce-…'`, when it's one an attribute can
/// carry (base64).
fn nonce_in(source: &str) -> Option<String> {
    let quoted = source.strip_prefix('\'')?.strip_suffix('\'')?;
    let (kind, value) = quoted.split_once('-')?;
    let base64 = |c: char| c.is_ascii_alphanumeric() || "+/=_-".contains(c);
    (kind.eq_ignore_ascii_case("nonce") && !value.is_empty() && value.chars().all(base64))
        .then(|| value.to_string())
}

/// A page's body, then the tag of Rovik's script, before any trailers.
struct WithScript {
    page: Incoming,
    /// To come once the page's body has: None once it has gone.
    tag: Option<Bytes>,
    /// The page's trailers, held until the tag has gone.
    trailers: Option<Frame<Bytes>>,
}

impl hyper::body::Body for WithScript {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, hyper::Error>>> {
        let this = self.get_mut();
        if this.tag.is_none() {
            if let Some(trailers) = this.trailers.take() {
                return Poll::Ready(Some(Ok(trailers)));
            }
            return Pin::new(&mut this.page).poll_frame(cx);
        }
        match ready!(Pin::new(&mut this.page).poll_frame(cx)) {
            Some(Ok(frame)) if !frame.is_data() => {
                this.trailers = Some(frame);
                Poll::Ready(this.tag.take().map(|tag| Ok(Frame::data(tag))))
            }
            None => Poll::Ready(this.tag.take().map(|tag| Ok(Frame::data(tag)))),
            frame => Poll::Ready(frame),
        }
    }
}

/// Lifts what keeps a page out of a frame: `X-Frame-Options`, and the
/// `frame-ancestors` of each CSP the server sent, its other rules kept.
/// Lifted, not narrowed to the app: `frame-ancestors *` doesn't even let
/// in the app's own `tauri://localhost`, and a narrower list is matched
/// differently by each webview. A report-only policy blocks nothing and
/// stays.
fn frameable(headers: &mut HeaderMap) {
    headers.remove(X_FRAME_OPTIONS);
    let policies: Vec<HeaderValue> = headers
        .get_all(CONTENT_SECURITY_POLICY)
        .iter()
        .filter_map(without_frame_ancestors)
        .collect();
    headers.remove(CONTENT_SECURITY_POLICY);
    for policy in policies {
        headers.append(CONTENT_SECURITY_POLICY, policy);
    }
}

/// The CSP header `value` without its `frame-ancestors` directives: none
/// when nothing else is left. A header of several policies (comma
/// separated) keeps each one that has another rule.
fn without_frame_ancestors(value: &HeaderValue) -> Option<HeaderValue> {
    // Not text: no directive could be read in it, as no browser reads one.
    let Ok(text) = value.to_str() else {
        return Some(value.clone());
    };
    let policies: Vec<String> = text
        .split(',')
        .filter_map(|policy| {
            let directives: Vec<&str> = policy
                .split(';')
                .map(str::trim)
                .filter(|directive| !directive.is_empty() && !is_frame_ancestors(directive))
                .collect();
            (!directives.is_empty()).then(|| directives.join("; "))
        })
        .collect();
    if policies.is_empty() {
        return None;
    }
    HeaderValue::from_str(&policies.join(", ")).ok()
}

fn is_frame_ancestors(directive: &str) -> bool {
    directive
        .split_ascii_whitespace()
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case("frame-ancestors"))
}

/// Each `Set-Cookie` of the response as a frame of another site keeps it
/// (`frame_cookie`).
fn frame_cookies_in(headers: &mut HeaderMap) {
    let cookies: Vec<HeaderValue> = headers
        .get_all(SET_COOKIE)
        .iter()
        .map(|cookie| {
            cookie
                .to_str()
                .ok()
                .and_then(|text| HeaderValue::from_str(&frame_cookie(text)).ok())
                .unwrap_or_else(|| cookie.clone())
        })
        .collect();
    headers.remove(SET_COOKIE);
    for cookie in cookies {
        headers.append(SET_COOKIE, cookie);
    }
}

/// `cookie`, a `Set-Cookie` value, as a frame of another site keeps it:
/// `SameSite=None` and `Secure` (which `localhost` takes over plain http),
/// in place of what it said of them, and `Partitioned`, so it stays the
/// app's tab's alone even where third-party cookies are blocked.
fn frame_cookie(cookie: &str) -> String {
    let mut parts = cookie.split(';').map(str::trim);
    let pair = parts.next().unwrap_or_default();
    let replaced = ["samesite", "secure", "partitioned"];
    let kept = parts.filter(|attribute| {
        let name = attribute.split('=').next().unwrap_or_default().trim();
        !attribute.is_empty() && !replaced.iter().any(|r| name.eq_ignore_ascii_case(r))
    });
    std::iter::once(pair)
        .chain(kept)
        .chain(["SameSite=None", "Secure", "Partitioned"])
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::Empty;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
    use tokio::sync::mpsc;

    type Connect = Box<dyn Fn() -> std::future::Ready<Result<Stream, String>> + Send + Sync>;
    type Played = Upstream<Connect, Empty<Bytes>>;

    /// An upstream to a server the test plays by hand: each stream it
    /// opens, the server's end of it comes to the test.
    fn played() -> (Arc<Played>, mpsc::UnboundedReceiver<DuplexStream>) {
        let (ends, servers) = mpsc::unbounded_channel();
        let connect: Connect = Box::new(move || {
            let (tab_side, server_side) = tokio::io::duplex(1 << 16);
            let _ = ends.send(server_side);
            std::future::ready(Ok(Box::new(tab_side) as Stream))
        });
        let upstream = Upstream {
            connect,
            sender: Mutex::new(None),
        };
        (Arc::new(upstream), servers)
    }

    fn get(path: &str) -> Request<Empty<Bytes>> {
        Request::get(path)
            .header("host", "localhost")
            .body(Empty::new())
            .unwrap()
    }

    /// How long a test waits for what it expects before it fails.
    const WAIT: Duration = Duration::from_secs(5);

    type Asking = tokio::task::JoinHandle<Result<Response<Incoming>, Unanswered>>;

    /// `request` on its way to the server through `upstream`.
    fn asking(upstream: &Arc<Played>, request: Request<Empty<Bytes>>) -> Asking {
        let upstream = upstream.clone();
        tokio::spawn(async move { upstream.send(request).await })
    }

    /// What a request came to, once it did.
    async fn outcome(asking: Asking) -> Result<Response<Incoming>, Unanswered> {
        let done = tokio::time::timeout(WAIT, asking).await;
        done.expect("an outcome in time").unwrap()
    }

    /// The server's end of the next stream opened to it.
    async fn next_stream(servers: &mut mpsc::UnboundedReceiver<DuplexStream>) -> DuplexStream {
        let opened = tokio::time::timeout(WAIT, servers.recv()).await;
        opened.expect("a stream opened in time").unwrap()
    }

    /// The head of the next request the server's end gets.
    async fn request_head(server: &mut DuplexStream) -> String {
        let reading = async {
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                assert_eq!(server.read(&mut byte).await.unwrap(), 1, "a whole request");
                head.push(byte[0]);
            }
            String::from_utf8(head).unwrap()
        };
        tokio::time::timeout(WAIT, reading)
            .await
            .expect("a request in time")
    }

    /// The server's end answers with `body`, keeping the stream open.
    async fn answer(server: &mut DuplexStream, body: &str) {
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        );
        server.write_all(response.as_bytes()).await.unwrap();
    }

    async fn text(response: Response<Incoming>) -> String {
        let body = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(body.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn a_tabs_requests_go_over_the_stream_kept_from_the_last() {
        let (upstream, mut servers) = played();
        let first = asking(&upstream, get("/a"));
        let mut server = next_stream(&mut servers).await;
        assert!(request_head(&mut server)
            .await
            .starts_with("GET /a HTTP/1.1"));
        answer(&mut server, "a").await;
        assert_eq!(text(outcome(first).await.unwrap()).await, "a");

        let second = asking(&upstream, get("/b"));
        assert!(request_head(&mut server)
            .await
            .starts_with("GET /b HTTP/1.1"));
        answer(&mut server, "b").await;
        assert_eq!(text(outcome(second).await.unwrap()).await, "b");
        assert!(servers.try_recv().is_err(), "no other stream was opened");
    }

    #[tokio::test]
    async fn a_request_after_the_server_closed_the_kept_stream_goes_over_a_new_one() {
        let (upstream, mut servers) = played();
        let first = asking(&upstream, get("/a"));
        let mut server = next_stream(&mut servers).await;
        request_head(&mut server).await;
        answer(&mut server, "a").await;
        text(outcome(first).await.unwrap()).await;
        // The server lets the idle stream go, and it's seen closed.
        drop(server);
        let mut seen = false;
        for _ in 0..500 {
            seen = upstream
                .sender
                .lock()
                .await
                .as_ref()
                .is_none_or(SendRequest::is_closed);
            if seen {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(seen, "the closed stream was never seen");

        let second = asking(&upstream, get("/b"));
        let mut server = next_stream(&mut servers).await;
        assert!(request_head(&mut server)
            .await
            .starts_with("GET /b HTTP/1.1"));
        answer(&mut server, "b").await;
        assert_eq!(text(outcome(second).await.unwrap()).await, "b");
    }

    #[tokio::test]
    async fn a_request_the_server_drops_on_a_kept_stream_goes_unanswered_for_the_browser_to_resend()
    {
        let (upstream, mut servers) = played();
        let first = asking(&upstream, get("/a"));
        let mut server = next_stream(&mut servers).await;
        request_head(&mut server).await;
        answer(&mut server, "a").await;
        text(outcome(first).await.unwrap()).await;

        let second = asking(&upstream, get("/b"));
        request_head(&mut server).await;
        drop(server);
        assert!(matches!(outcome(second).await, Err(Unanswered::Dropped)));
        assert!(servers.try_recv().is_err(), "not sent again here");
    }

    #[tokio::test]
    async fn a_new_stream_the_server_closes_unanswered_says_so() {
        let (upstream, mut servers) = played();
        let first = asking(&upstream, get("/a"));
        let mut server = next_stream(&mut servers).await;
        request_head(&mut server).await;
        drop(server);
        let Err(Unanswered::Unreachable(why)) = outcome(first).await else {
            panic!("unreachable, with why");
        };
        assert!(why.starts_with("its server didn't answer"), "{why}");
    }

    #[tokio::test]
    async fn a_server_out_of_reach_is_shown_why_in_the_tab() {
        let connect =
            || std::future::ready(Err::<Stream, _>("Nothing listens on port 5173".into()));
        let upstream: Upstream<_, Empty<Bytes>> = Upstream {
            connect,
            sender: Mutex::new(None),
        };
        let response = upstream.relay(get("/"), false).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            body,
            "Rovik couldn't show this page: Nothing listens on port 5173"
        );
    }

    #[tokio::test]
    async fn a_relayed_answer_is_framable_and_its_cookies_kept_by_a_frame_where_asked() {
        for frame_cookies in [false, true] {
            let (upstream, mut servers) = played();
            let relaying = {
                let upstream = upstream.clone();
                tokio::spawn(async move { upstream.relay(get("/"), frame_cookies).await })
            };
            let mut server = next_stream(&mut servers).await;
            request_head(&mut server).await;
            let response = "HTTP/1.1 200 OK\r\nx-frame-options: DENY\r\n\
                content-security-policy: default-src 'self'; frame-ancestors 'none'\r\n\
                set-cookie: sessionid=abc; SameSite=Lax\r\ncontent-length: 2\r\n\r\nhi";
            server.write_all(response.as_bytes()).await.unwrap();
            let relayed = relaying.await.unwrap().unwrap();
            let headers = relayed.headers();
            assert!(headers.get(X_FRAME_OPTIONS).is_none());
            assert_eq!(headers[CONTENT_SECURITY_POLICY], "default-src 'self'");
            let cookie = match frame_cookies {
                true => "sessionid=abc; SameSite=None; Secure; Partitioned",
                false => "sessionid=abc; SameSite=Lax",
            };
            assert_eq!(headers[SET_COOKIE], cookie);
            let body = relayed.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(body, "hi");
        }
    }

    /// A request for `path` with `headers`, as a browser's.
    fn request(path: &str, headers: &[(&str, &str)]) -> Request<Empty<Bytes>> {
        let mut request = get(path);
        for (name, value) in headers {
            let name = hyper::header::HeaderName::from_bytes(name.as_bytes()).unwrap();
            request
                .headers_mut()
                .append(name, HeaderValue::from_str(value).unwrap());
        }
        request
    }

    /// A page the tab's frame loads, as WebKit and WebView2 ask for one.
    fn page_load(path: &str) -> Request<Empty<Bytes>> {
        request(
            path,
            &[
                ("sec-fetch-dest", "iframe"),
                ("accept", "text/html,application/xhtml+xml,*/*;q=0.8"),
                ("accept-encoding", "gzip, deflate, br"),
            ],
        )
    }

    /// An HTTP/1.1 answer with `headers` and `body`, its length said.
    fn answered(headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\n{headers}content-length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    /// `request` relayed to a played server that answers `answer`: the head
    /// of the request it got, and the response the tab gets.
    async fn relayed(request: Request<Empty<Bytes>>, answer: &str) -> (String, Response<Body>) {
        let (upstream, mut servers) = played();
        let relaying = {
            let upstream = upstream.clone();
            tokio::spawn(async move { upstream.relay(request, false).await })
        };
        let mut server = next_stream(&mut servers).await;
        let head = request_head(&mut server).await;
        server.write_all(answer.as_bytes()).await.unwrap();
        let response = relaying.await.unwrap().unwrap();
        (head, response)
    }

    async fn body_of(response: Response<Body>) -> String {
        let body = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(body.to_vec()).unwrap()
    }

    const TAG: &str = "<script src=\"/__rovik/tab.js\"></script>";

    #[tokio::test]
    async fn a_page_the_tab_loads_comes_uncompressed_with_rovik_s_script_after_it() {
        let html = "text/html; charset=utf-8";
        let answer = answered(&format!("content-type: {html}\r\n"), "<p>hi</p>");
        let (asked, response) = relayed(page_load("/about"), &answer).await;
        assert!(asked.contains("accept-encoding: identity\r\n"), "{asked}");
        assert!(!asked.contains("gzip"), "{asked}");
        assert!(
            response.headers().get(CONTENT_LENGTH).is_none(),
            "longer now"
        );
        assert_eq!(response.headers()[CONTENT_TYPE], html);
        assert_eq!(body_of(response).await, format!("<p>hi</p>{TAG}"));
    }

    /// WebKit before Sec-Fetch-Dest: a page load is one that takes HTML
    /// first.
    #[tokio::test]
    async fn without_sec_fetch_dest_a_page_load_is_one_that_takes_html_first() {
        let answer = answered("content-type: text/html\r\n", "<p>hi</p>");
        let navigation = request("/", &[("accept", "text/html,*/*;q=0.8")]);
        let (_, response) = relayed(navigation, &answer).await;
        assert_eq!(body_of(response).await, format!("<p>hi</p>{TAG}"));
        let fetch = request("/", &[("accept", "*/*")]);
        let (_, response) = relayed(fetch, &answer).await;
        assert_eq!(body_of(response).await, "<p>hi</p>");
    }

    #[tokio::test]
    async fn what_isnt_a_page_load_of_plain_html_goes_as_it_came() {
        let fetch = || {
            request(
                "/",
                &[
                    ("sec-fetch-dest", "empty"),
                    ("accept", "text/html"),
                    ("accept-encoding", "gzip"),
                ],
            )
        };
        let html = "content-type: text/html\r\n";
        let cases = [
            // A script's fetch of HTML (htmx): asked for as the script asked.
            (fetch(), answered(html, "<p>x</p>"), "<p>x</p>"),
            (
                page_load("/"),
                answered("content-type: application/json\r\n", "{}"),
                "{}",
            ),
            (
                page_load("/"),
                answered(
                    "content-type: text/html\r\ncontent-encoding: gzip\r\n",
                    "zz",
                ),
                "zz",
            ),
            (
                page_load("/"),
                answered(
                    "content-type: text/html\r\ncontent-disposition: attachment\r\n",
                    "<p>x</p>",
                ),
                "<p>x</p>",
            ),
            (
                page_load("/"),
                answered("content-type: text/html; charset=UTF-16\r\n", "xx"),
                "xx",
            ),
            (
                page_load("/"),
                "HTTP/1.1 304 Not Modified\r\ncontent-type: text/html\r\n\r\n".to_string(),
                "",
            ),
        ];
        for (request, answer, body) in cases {
            let fetched = request.headers().get("sec-fetch-dest").unwrap() == "empty";
            let (asked, response) = relayed(request, &answer).await;
            assert_eq!(asked.contains("accept-encoding: gzip"), fetched, "{asked}");
            assert_eq!(body_of(response).await, body, "{answer}");
        }
    }

    #[tokio::test]
    async fn rovik_s_script_is_this_port_s_own_and_never_reaches_the_server() {
        let (upstream, mut servers) = played();
        let response = upstream.relay(get("/__rovik/tab.js"), false).await.unwrap();
        assert_eq!(
            response.headers()[CONTENT_TYPE],
            "text/javascript; charset=utf-8"
        );
        assert_eq!(body_of(response).await, include_str!("tab.js"));
        assert!(servers.try_recv().is_err(), "no stream to the server");
    }

    #[tokio::test]
    async fn the_script_s_tag_carries_the_nonce_the_page_s_csp_asks_scripts_for() {
        let csp = "content-security-policy: default-src 'self'; script-src 'nonce-r4nd0m+/=' \
                   'strict-dynamic'\r\ncontent-type: text/html\r\n";
        let (_, response) = relayed(page_load("/"), &answered(csp, "<p>x</p>")).await;
        assert_eq!(
            body_of(response).await,
            "<p>x</p><script src=\"/__rovik/tab.js\" nonce=\"r4nd0m+/=\"></script>"
        );
    }

    #[test]
    fn the_nonce_is_the_one_of_the_directive_that_governs_a_script_tag() {
        let nonce = |csp: &str| script_nonce(&headers(&[("content-security-policy", csp)]));
        assert_eq!(nonce("script-src 'nonce-abc'").as_deref(), Some("abc"));
        assert_eq!(nonce("default-src 'NONCE-d1'").as_deref(), Some("d1"));
        assert_eq!(
            nonce("default-src 'nonce-d'; script-src-elem 'nonce-e' 'self'").as_deref(),
            Some("e")
        );
        // script-src-elem governs a script tag: its 'self' lets the tag in.
        assert_eq!(nonce("script-src-elem 'self'; script-src 'nonce-s'"), None);
        assert_eq!(nonce("img-src 'nonce-i'"), None);
        assert_eq!(
            nonce("script-src 'nonce-a\"b'"),
            None,
            "no attribute could carry it"
        );
        assert_eq!(nonce("script-src 'self'"), None);
        assert_eq!(script_nonce(&HeaderMap::new()), None);
    }

    /// The tag follows the page's body and goes before its trailers.
    #[tokio::test]
    async fn the_tag_goes_before_the_page_s_trailers() {
        let answer = "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ntransfer-encoding: chunked\r\n\
                      trailer: x-done\r\n\r\n5\r\nhello\r\n0\r\nx-done: 1\r\n\r\n";
        let (_, response) = relayed(page_load("/"), answer).await;
        let collected = response.into_body().collect().await.unwrap();
        let trailers = collected.trailers().cloned();
        assert_eq!(collected.to_bytes(), format!("hello{TAG}"));
        assert_eq!(trailers.expect("the page's trailers")["x-done"], "1");
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    fn values(map: &HeaderMap, name: &str) -> Vec<String> {
        map.get_all(name)
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn x_frame_options_is_lifted_whatever_it_says() {
        for said in ["DENY", "SAMEORIGIN", "allow-from https://example.com"] {
            let mut map = headers(&[("x-frame-options", said), ("content-type", "text/html")]);
            frameable(&mut map);
            assert!(map.get(X_FRAME_OPTIONS).is_none(), "{said}");
            assert_eq!(values(&map, "content-type"), ["text/html"]);
        }
    }

    #[test]
    fn frame_ancestors_goes_from_a_policy_and_its_other_rules_stay() {
        let mut map = headers(&[(
            "content-security-policy",
            "default-src 'self'; Frame-Ancestors 'none' ;script-src 'self' 'nonce-a1'",
        )]);
        frameable(&mut map);
        assert_eq!(
            values(&map, "content-security-policy"),
            ["default-src 'self'; script-src 'self' 'nonce-a1'"]
        );
    }

    #[test]
    fn a_policy_of_frame_ancestors_alone_goes_and_the_others_stay() {
        let mut map = headers(&[
            ("content-security-policy", "frame-ancestors 'self'"),
            (
                "content-security-policy",
                "frame-ancestors 'none', img-src 'self'; frame-ancestors 'self'",
            ),
            ("content-security-policy", "upgrade-insecure-requests"),
        ]);
        frameable(&mut map);
        assert_eq!(
            values(&map, "content-security-policy"),
            ["img-src 'self'", "upgrade-insecure-requests"]
        );
    }

    #[test]
    fn a_report_only_policy_and_a_policy_without_frame_ancestors_stay_as_they_were() {
        let mut map = headers(&[
            (
                "content-security-policy-report-only",
                "frame-ancestors 'none'",
            ),
            ("content-security-policy", "default-src 'self'; img-src *"),
        ]);
        frameable(&mut map);
        assert_eq!(
            values(&map, "content-security-policy-report-only"),
            ["frame-ancestors 'none'"]
        );
        assert_eq!(
            values(&map, "content-security-policy"),
            ["default-src 'self'; img-src *"]
        );
    }

    #[test]
    fn a_cookie_takes_samesite_none_secure_and_partitioned_in_place_of_its_own() {
        assert_eq!(
            frame_cookie("sessionid=abc; HttpOnly; Path=/; SameSite=Lax"),
            "sessionid=abc; HttpOnly; Path=/; SameSite=None; Secure; Partitioned"
        );
        assert_eq!(
            frame_cookie("a=1; secure; samesite=strict; partitioned; Max-Age=0"),
            "a=1; Max-Age=0; SameSite=None; Secure; Partitioned"
        );
        assert_eq!(
            frame_cookie("csrftoken=x; expires=Sat, 23 Oct 2027 10:00:00 GMT"),
            "csrftoken=x; expires=Sat, 23 Oct 2027 10:00:00 GMT; SameSite=None; Secure; Partitioned"
        );
    }

    #[test]
    fn every_cookie_of_a_response_is_rewritten() {
        let mut map = headers(&[
            ("set-cookie", "a=1; Path=/"),
            ("set-cookie", "b=2; SameSite=Lax"),
        ]);
        frame_cookies_in(&mut map);
        assert_eq!(
            values(&map, "set-cookie"),
            [
                "a=1; Path=/; SameSite=None; Secure; Partitioned",
                "b=2; SameSite=None; Secure; Partitioned"
            ]
        );
    }
}
