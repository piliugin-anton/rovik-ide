//! The MCP servers the user set up in Settings → MCP servers, whose tools
//! the agent calls with its `mcp` tool (docs/design/mcp-plan.md). Rovik is
//! the client here; connectors/mcp.rs is the other way round, a server of
//! Rovik's own tools for Claude Code and Codex.
//!
//!   rpc.rs    JSON-RPC as MCP speaks it, and its two protocol eras
//!   stdio.rs  a command's server, on its stdin and stdout
//!   http.rs   a URL's server (Streamable HTTP)
//!   tool.rs   a server's tools as `tools/list` gives them
//!
//! [`McpState`], one of the core's parts, starts each server when a run
//! first needs it and keeps it running while the app is open (Q04). Only
//! tools: no resources, prompts, sampling, roots or elicitation (Q03).
//!
//! On a daemon, a URL server on the loopback is on the user's computer,
//! which the host can't reach: its POSTs go through the app there, a
//! [`Relay`] (mcp-plan-2.md, Q11).

mod http;
mod rpc;
mod stdio;
#[cfg(test)]
mod tests;
mod tool;

pub use rovik_protocol::mcp::{is_valid_name, McpServer, McpTransport, McpValue};
pub use tool::Tool;

use crate::agent_run::StopSignal;
use futures_util::future::BoxFuture;
use rovik_protocol::mcp::RelayedPost;
use rovik_protocol::Secret;
use rpc::{Era, Failure};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a server has to start and answer the handshake: an `npx`
/// server may download itself first.
#[cfg(not(any(test, feature = "testing")))]
const START_WITHIN: Duration = Duration::from_secs(60);
#[cfg(any(test, feature = "testing"))]
const START_WITHIN: Duration = Duration::from_secs(10);
/// How long listing a server's tools may take.
#[cfg(not(any(test, feature = "testing")))]
const LIST_WITHIN: Duration = Duration::from_secs(30);
#[cfg(any(test, feature = "testing"))]
const LIST_WITHIN: Duration = Duration::from_secs(5);
/// How long a call may take.
const CALL_WITHIN: Duration = Duration::from_secs(10 * 60);
/// How long a server that failed waits to be started again.
#[cfg(not(any(test, feature = "testing")))]
const RETRY_AFTER: Duration = Duration::from_secs(60);
#[cfg(any(test, feature = "testing"))]
const RETRY_AFTER: Duration = Duration::from_millis(300);
/// The most pages of tools read from one server.
const MAX_PAGES: usize = 50;

/// Where a server's secret values come from: the app's keychain (Q02).
/// Given the server's id and the value's key (`env/NAME`, `header/Name`),
/// its value, or none when it isn't saved. Called on a blocking thread.
pub type SecretSource = Arc<dyn Fn(&str, &str) -> Result<Option<String>, String> + Send + Sync>;

/// How a daemon reaches the URL servers on the loopback of its user's
/// computers, which it can't reach itself: through the app on each
/// (mcp-plan-2.md, Q11).
pub trait Relay: Send + Sync {
    /// The computer whose loopback `chat`'s URL servers mean: the one the
    /// chat belongs to; none for a chat of no computer.
    fn computer_of(&self, chat: &str) -> Option<String>;
    /// Has the app on `computer` make `post`: its answer as it comes, or
    /// why it couldn't (the computer is away, or its app refused).
    fn post(
        &self,
        computer: &str,
        post: RelayedPost,
    ) -> BoxFuture<'static, Result<reqwest::Response, String>>;
}

/// A relay, and the computer it posts on.
type Relayed = (Arc<dyn Relay>, String);

/// A relayed POST's answer as the transport reads it (Q11): `status` and
/// `headers` as the computer got them (one HTTP can't carry is left out),
/// and its body as `body` brings it, text by text; an error there ends it,
/// as a broken connection would.
pub fn relayed_response(
    status: u16,
    headers: &[(String, String)],
    body: impl futures_util::Stream<Item = Result<String, String>> + Send + 'static,
) -> Result<reqwest::Response, String> {
    use futures_util::StreamExt;
    use reqwest::header::{HeaderName, HeaderValue};
    let mut built = ::http::Response::builder().status(status);
    for (name, value) in headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            built = built.header(name, value);
        }
    }
    let body = body.map(|part| part.map(bytes::Bytes::from).map_err(std::io::Error::other));
    let response = built
        .body(reqwest::Body::wrap_stream(body))
        .map_err(|e| format!("The answer from the user's computer can't be read: {e}"))?;
    Ok(reqwest::Response::from(response))
}

/// What a server offers a run (Q06).
#[derive(Clone, Debug, PartialEq)]
pub struct Listed {
    /// Its name in the settings, which the agent calls it by.
    pub name: String,
    /// What the server tells the model of itself (its `instructions`).
    pub instructions: String,
    /// Its tools, in name order, the user's switched-off ones left out;
    /// or why it's unavailable.
    pub tools: Result<Vec<Tool>, String>,
}

pub use rovik_protocol::mcp::{Tested, TestedTool};

/// A call that didn't come back with a result.
#[derive(Clone, Debug, PartialEq)]
pub struct CallError {
    /// What went wrong, for the agent.
    pub text: String,
    /// The server refused the arguments (`-32602`): the agent gets the
    /// tool's schema with the error.
    pub bad_arguments: bool,
}

/// Waits for `future` up to `within`, or until `stop` is pressed.
async fn wait_for<T>(
    future: impl Future<Output = T>,
    within: Duration,
    stop: Option<&StopSignal>,
) -> Result<T, Failure> {
    let Some(stop) = stop else {
        return tokio::time::timeout(within, future)
            .await
            .map_err(|_| Failure::TimedOut);
    };
    let mut stopped = stop.subscribe();
    if *stopped.borrow() {
        return Err(Failure::Stopped);
    }
    tokio::select! {
        done = tokio::time::timeout(within, future) => done.map_err(|_| Failure::TimedOut),
        _ = stopped.wait_for(|stopped| *stopped) => Err(Failure::Stopped),
    }
}

/// How a session reaches its server.
enum Link {
    Stdio(stdio::Stdio),
    Http(http::Http),
}

/// A server spoken to: its link, the era it speaks, and what it says of
/// itself.
struct Session {
    link: Link,
    era: Era,
    /// Its name and version, as it gives them.
    server: String,
    instructions: String,
}

/// The headers a request goes with on HTTP: its protocol version; and a
/// modern request's method, the name it's about, and its `Mcp-Param-…`.
fn headers_for(
    era: &Era,
    method: &str,
    name: Option<&str>,
    params: Vec<(String, String)>,
) -> Vec<(String, String)> {
    let mut headers = vec![(
        "MCP-Protocol-Version".to_string(),
        era.version().to_string(),
    )];
    if *era == Era::Modern {
        headers.push(("Mcp-Method".into(), method.into()));
        if let Some(name) = name {
            headers.push(("Mcp-Name".into(), http::header_value(name)));
        }
        headers.extend(params);
    }
    headers
}

impl Link {
    /// A request to the server. Boxed, as `notify` and `stop` are: the
    /// transports' futures hold reqwest's types and a child process's, so
    /// deep that a run's future holding them couldn't be proven `Send`
    /// (rustc's E0275, building rovik-agentd).
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Value,
        headers: &'a [(String, String)],
        within: Duration,
        stop: Option<&'a StopSignal>,
    ) -> BoxFuture<'a, Result<Value, Failure>> {
        match self {
            Link::Stdio(stdio) => Box::pin(stdio.request(method, params, within, stop)),
            Link::Http(http) => Box::pin(http.request(method, params, headers, within, stop)),
        }
    }

    fn notify<'a>(
        &'a self,
        method: &'a str,
        params: Value,
        headers: &'a [(String, String)],
    ) -> BoxFuture<'a, ()> {
        match self {
            Link::Stdio(stdio) => Box::pin(async move { stdio.notify(method, params) }),
            Link::Http(http) => Box::pin(http.notify(method, params, headers)),
        }
    }

    /// Ends a command's server, and what it started; a URL's has nothing
    /// of Rovik's to end.
    fn stop(&self) -> BoxFuture<'_, ()> {
        match self {
            Link::Stdio(stdio) => Box::pin(stdio.stop()),
            Link::Http(_) => Box::pin(async {}),
        }
    }

    /// The text of an error in starting a server, or in testing it, a
    /// command server's stderr after it when the error doesn't carry it
    /// already (one whose process ended does).
    fn explained(&self, text: String) -> String {
        match self {
            Link::Stdio(stdio) if stdio.ended().is_none() => {
                let stderr = stdio.stderr();
                if stderr.is_empty() {
                    text
                } else {
                    format!("{text}. It said:\n{stderr}")
                }
            }
            _ => text,
        }
    }

    /// Whether it can still be spoken to.
    fn alive(&self) -> bool {
        match self {
            Link::Stdio(stdio) => stdio.ended().is_none(),
            Link::Http(_) => true,
        }
    }
}

/// `initialize`'s result over `link`, and the version it agreed on, then
/// `notifications/initialized` (a legacy server's handshake).
async fn initialize(link: &Link) -> Result<(Value, String), Failure> {
    let params = json!({
        "protocolVersion": rpc::LEGACY,
        "capabilities": {},
        "clientInfo": rpc::client_info(),
    });
    let result = link
        .request("initialize", params, &[], START_WITHIN, None)
        .await?;
    let version = result
        .get("protocolVersion")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .unwrap_or(rpc::LEGACY)
        .to_string();
    let headers = headers_for(&Era::Legacy(version.clone()), "", None, Vec::new());
    link.notify("notifications/initialized", json!({}), &headers)
        .await;
    Ok((result, version))
}

/// What a server's info says of it: its name and version.
fn named(info: Option<&Value>) -> String {
    let field = |key: &str| {
        info.and_then(|i| i.get(key))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    [field("name"), field("version")]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn instructions_of(result: &Value) -> String {
    result
        .get("instructions")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

impl Session {
    /// Speaks to the server over `link` (Q03): the handshake first, the
    /// stateless protocol when the server refuses it.
    async fn open(link: Link) -> Result<Session, String> {
        let refused = match initialize(&link).await {
            Ok((result, version)) => {
                return Ok(Session {
                    server: named(result.get("serverInfo")),
                    instructions: instructions_of(&result),
                    era: Era::Legacy(version),
                    link,
                })
            }
            Err(failure) => failure,
        };
        if let Some(why) = final_failure(&refused) {
            return Err(link.explained(why));
        }
        let params = rpc::params_for(&Era::Modern, json!({}));
        let headers = headers_for(&Era::Modern, "server/discover", None, Vec::new());
        let discovered = link
            .request("server/discover", params, &headers, START_WITHIN, None)
            .await;
        let result = match discovered {
            Ok(result) => result,
            // Not a modern server either: the handshake's error is its.
            Err(_) => return Err(link.explained(refused_text(&refused))),
        };
        let versions: Vec<&str> = result
            .get("supportedVersions")
            .and_then(Value::as_array)
            .map(|v| v.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if !versions.contains(&rpc::MODERN) {
            return Err(format!(
                "The server speaks MCP {}; Rovik speaks {} and the versions before it.",
                versions.join(", "),
                rpc::MODERN
            ));
        }
        let info = result
            .get("_meta")
            .and_then(|m| m.get("io.modelcontextprotocol/serverInfo"));
        Ok(Session {
            server: named(info),
            instructions: instructions_of(&result),
            era: Era::Modern,
            link,
        })
    }

    /// A request in the session's era: a legacy HTTP session that's gone
    /// (its 404) is started again, once, and the request sent again.
    async fn request(
        &self,
        method: &str,
        params: Value,
        name: Option<&str>,
        header_params: Vec<(String, String)>,
        within: Duration,
        stop: Option<&StopSignal>,
    ) -> Result<Value, Failure> {
        let params = rpc::params_for(&self.era, params);
        let headers = headers_for(&self.era, method, name, header_params);
        let answer = self
            .link
            .request(method, params.clone(), &headers, within, stop)
            .await;
        let Link::Http(http) = &self.link else {
            return answer;
        };
        match answer {
            Err(Failure::Http { status: 404, .. }) if http.has_session() => {
                http.end_session();
                initialize(&self.link).await?;
                self.link
                    .request(method, params, &headers, within, stop)
                    .await
            }
            answer => answer,
        }
    }

    /// Its tools, every page of them; with a modern HTTP server, those
    /// whose `x-mcp-header` is invalid left out.
    async fn list_tools(&self) -> Result<Vec<Tool>, Failure> {
        let checks_headers = self.era == Era::Modern && matches!(self.link, Link::Http(_));
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen = HashSet::new();
        for _ in 0..MAX_PAGES {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let result = self
                .request("tools/list", params, None, Vec::new(), LIST_WITHIN, None)
                .await?;
            tools.extend(tool::tools_of(&result, checks_headers));
            match result.get("nextCursor").and_then(Value::as_str) {
                Some(next) if !next.is_empty() && seen.insert(next.to_string()) => {
                    cursor = Some(next.to_string())
                }
                _ => break,
            }
        }
        Ok(tools)
    }

    /// Calls `tool` with `arguments`: the server's result (a
    /// `CallToolResult`).
    async fn call(
        &self,
        tool: &Tool,
        arguments: Value,
        within: Duration,
        stop: Option<&StopSignal>,
    ) -> Result<Value, Failure> {
        let header_params = match (&self.era, &self.link) {
            (Era::Modern, Link::Http(_)) => tool::header_values(tool, &arguments),
            _ => Vec::new(),
        };
        let params = json!({ "name": tool.name, "arguments": arguments });
        self.request(
            "tools/call",
            params,
            Some(&tool.name),
            header_params,
            within,
            stop,
        )
        .await
    }

    async fn stop(&self) {
        self.link.stop().await;
    }
}

/// The handshake's failure when it settles the matter, with the text to
/// show: no answer, a server that ended or can't be reached, credentials
/// refused. Otherwise the server may be a modern one: `server/discover`
/// asks.
fn final_failure(failure: &Failure) -> Option<String> {
    match failure {
        Failure::TimedOut => Some(format!(
            "The server didn't answer within {} seconds.",
            START_WITHIN.as_secs()
        )),
        Failure::Stopped => Some("Stopped.".into()),
        Failure::Gone(why) => Some(why.clone()),
        Failure::Http {
            status: 401 | 403,
            ..
        } => Some(format!(
            "The server refused Rovik's credentials ({}): check its headers in Settings → MCP servers.",
            failure.text()
        )),
        _ => None,
    }
}

/// What the handshake's refusal says, for a server that isn't a modern one
/// either. A 404 or 405 may be the old SSE transport's server.
fn refused_text(failure: &Failure) -> String {
    match failure {
        Failure::Http {
            status: 404 | 405, ..
        } => format!(
            "The server refused Rovik's handshake ({}). If it only speaks the old SSE \
             transport, start it through a bridge command: npx mcp-remote <url>.",
            failure.text()
        ),
        _ => format!("The server refused Rovik's handshake: {}", failure.text()),
    }
}

/// A server for the settings it was started with: its link's settings and
/// folder, and where it is.
struct Instance {
    transport: McpTransport,
    state: tokio::sync::Mutex<State>,
    /// When a list or call last began or ended on it: how long it has
    /// been idle.
    last_used: Mutex<Instant>,
    /// How many lists and calls are running on it now.
    in_use: AtomicUsize,
}

/// A list or call using an instance: while it lives, the instance isn't
/// idle. Taken under the `servers` lock that finds the instance, so a
/// sweep can't stop it between the two.
struct Busy(Arc<Instance>);

impl Busy {
    fn new(instance: Arc<Instance>) -> Busy {
        instance.in_use.fetch_add(1, Ordering::SeqCst);
        *lock(&instance.last_used) = Instant::now();
        Busy(instance)
    }
}

impl std::ops::Deref for Busy {
    type Target = Instance;
    fn deref(&self) -> &Instance {
        &self.0
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        *lock(&self.0.last_used) = Instant::now();
        self.0.in_use.fetch_sub(1, Ordering::SeqCst);
    }
}

enum State {
    /// Not started, or ended: started at its next use.
    Down,
    Up(Arc<Session>),
    /// It failed, at that moment: not started again before `RETRY_AFTER`.
    Failed {
        at: Instant,
        error: String,
    },
}

/// A server where it runs: its id, a command's folder, and the computer a
/// relayed URL server is on (Q11).
type Key = (String, Option<PathBuf>, Option<String>);

/// The servers of this process (Q04).
#[derive(Clone, Default)]
pub struct McpState(Arc<Shared>);

#[derive(Default)]
struct Shared {
    secrets: Mutex<Option<SecretSource>>,
    /// A daemon's way to the servers on its user's computers' loopback.
    relay: Mutex<Option<Arc<dyn Relay>>>,
    servers: Mutex<HashMap<Key, Arc<Instance>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether `url` is on the loopback: `localhost`, 127.0.0.0/8 or `[::1]`.
pub fn on_loopback(url: &str) -> bool {
    let Some(host) = reqwest::Url::parse(url.trim())
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
    else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// The folder a command's server starts in: the project's, or the user's
/// home without one (Q03).
fn folder_for(transport: &McpTransport, project: Option<&Path>) -> Option<PathBuf> {
    match transport {
        McpTransport::Url { .. } => None,
        McpTransport::Command { .. } => project.map(Path::to_path_buf).or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
        }),
    }
}

impl McpState {
    /// Where the servers' secrets come from: the app's keychain.
    pub fn set_secrets(&self, source: SecretSource) {
        *lock(&self.0.secrets) = Some(source);
    }

    /// How a daemon reaches the URL servers on its user's computers'
    /// loopback (Q11). The app has none: its loopback is the user's.
    pub fn set_relay(&self, relay: Arc<dyn Relay>) {
        *lock(&self.0.relay) = Some(relay);
    }

    /// The computer whose loopback `chat`'s URL servers mean, on a daemon
    /// that relays; none in the app.
    pub fn via(&self, chat: &str) -> Option<String> {
        let relay = lock(&self.0.relay).clone();
        relay?.computer_of(chat)
    }

    /// How a URL server at `url` is reached for a run or Test `via` a
    /// computer: through the relay, when this daemon has one and the URL is
    /// on the loopback; else straight. A loopback URL with no computer to
    /// go through can't be reached.
    fn relayed(&self, url: &str, via: Option<&str>) -> Result<Option<Relayed>, String> {
        let Some(relay) = lock(&self.0.relay).clone().filter(|_| on_loopback(url)) else {
            return Ok(None);
        };
        match via {
            Some(computer) => Ok(Some((relay, computer.to_string()))),
            None => Err(
                "The server is on the loopback of a user's computer, and this chat belongs to \
                 none: it can't be reached from here."
                    .into(),
            ),
        }
    }

    /// Where `server` runs for a run or Test in `project`, `via` a
    /// computer: a command's folder, a relayed URL's computer.
    fn key(&self, server: &McpServer, project: Option<&Path>, via: Option<&str>) -> Key {
        let computer = match &server.transport {
            McpTransport::Url { url, .. } => self.relayed(url, via).ok().flatten().map(|(_, c)| c),
            McpTransport::Command { .. } => None,
        };
        let folder = folder_for(&server.transport, project);
        (server.id.clone(), folder, computer)
    }

    /// The settings as they are now: a server removed, turned off, or whose
    /// transport changed, is stopped; the next run that needs it starts it
    /// again (Q04).
    pub fn configure(&self, servers: &[McpServer]) {
        let kept = |id: &str, transport: &McpTransport| {
            servers
                .iter()
                .any(|s| s.id == id && s.enabled && s.transport == *transport)
        };
        let gone = self.take(|(id, _, _), instance| !kept(id, &instance.transport));
        stop_all(gone);
    }

    /// The server's secrets changed: it starts again, with them, at its
    /// next use.
    pub fn forget(&self, id: &str) {
        let gone = self.take(|(i, _, _), _| i == id);
        stop_all(gone);
    }

    /// The saved secret values of the `servers` that have a secret field,
    /// on or off, by server id and key (`env/NAME` or `header/Name`), for a
    /// daemon that has no keychain (Q11): a start's servers, which are on,
    /// or the one Test tries. A server with none saved has an empty map, so
    /// a secret cleared here clears on the host too. A server with a value
    /// that can't be read is left out: the host keeps what it had. Read on
    /// a blocking thread.
    pub async fn saved_secrets(
        &self,
        servers: &[McpServer],
    ) -> BTreeMap<String, BTreeMap<String, Secret>> {
        let source = lock(&self.0.secrets).clone();
        let servers = servers.to_vec();
        let Some(source) = source else {
            return BTreeMap::new();
        };
        let read = crate::rt::spawn_blocking(move || {
            let mut all = BTreeMap::new();
            for server in &servers {
                let keys = server.transport.secret_keys();
                if keys.is_empty() {
                    continue;
                }
                let mut found = BTreeMap::new();
                let mut readable = true;
                for key in keys {
                    match source(&server.id, &key) {
                        Ok(Some(secret)) => {
                            found.insert(key, Secret(secret));
                        }
                        Ok(None) => {}
                        Err(e) => {
                            log::warn!(
                                "MCP server {}: the saved value {key} couldn't be read: {e}",
                                server.id
                            );
                            readable = false;
                        }
                    }
                }
                if readable {
                    all.insert(server.id.clone(), found);
                }
            }
            all
        })
        .await;
        read.unwrap_or_else(|e| {
            log::warn!("MCP servers' saved secrets couldn't be read: {e}");
            BTreeMap::new()
        })
    }

    /// Stops the servers not used for `unused_for` and not in use now: a
    /// daemon's sweep, since nothing else ends its servers (Q11). The next
    /// use starts one again.
    pub fn stop_idle(&self, unused_for: Duration) {
        let gone = self.take(|_, instance| {
            instance.in_use.load(Ordering::SeqCst) == 0
                && lock(&instance.last_used).elapsed() >= unused_for
        });
        stop_all(gone);
    }

    /// Stops the command servers started in `folder`: a chat's worktree
    /// that went (Q11).
    pub fn stop_in(&self, folder: &Path) {
        let gone = self.take(|(_, at, _), _| at.as_deref() == Some(folder));
        stop_all(gone);
    }

    /// Removes the instances `which` picks, by key and instance, from the
    /// running ones.
    fn take(&self, which: impl Fn(&Key, &Instance) -> bool) -> Vec<Arc<Instance>> {
        let mut running = lock(&self.0.servers);
        let keys: Vec<Key> = running
            .iter()
            .filter(|(key, instance)| which(key, instance))
            .map(|(key, _)| key.clone())
            .collect();
        keys.iter().filter_map(|key| running.remove(key)).collect()
    }

    /// The tools of the enabled `servers`, for a run whose project is
    /// `project`, its loopback URLs `via` that computer on a daemon (Q04,
    /// Q06, Q11): each server started if it isn't, side by side. Servers in
    /// name order, each one's tools too.
    pub async fn list(
        &self,
        servers: &[McpServer],
        project: Option<&Path>,
        via: Option<&str>,
    ) -> Vec<Listed> {
        let enabled = servers
            .iter()
            .filter(|s| s.enabled && is_valid_name(&s.name));
        let listed = enabled.map(|server| self.list_one(server, project, via));
        let mut listed = futures_util::future::join_all(listed).await;
        listed.sort_by(|a, b| a.name.cmp(&b.name));
        listed.dedup_by(|a, b| a.name == b.name);
        listed
    }

    async fn list_one(
        &self,
        server: &McpServer,
        project: Option<&Path>,
        via: Option<&str>,
    ) -> Listed {
        let instance = self.instance(server, project, via);
        let mut instructions = String::new();
        let tools = match self.session(&instance, server, project, via, None).await {
            Err(error) => Err(error),
            Ok(session) => {
                instructions = session.instructions.clone();
                match session.list_tools().await {
                    Ok(mut tools) => {
                        tools.retain(|t| !server.disabled_tools.contains(&t.name));
                        tools.sort_by(|a, b| a.name.cmp(&b.name));
                        tools.dedup_by(|a, b| a.name == b.name);
                        Ok(tools)
                    }
                    Err(failure) => {
                        let error = failure.text();
                        failed(&instance, &session, &error).await;
                        Err(error)
                    }
                }
            }
        };
        Listed {
            name: server.name.clone(),
            instructions,
            tools,
        }
    }

    /// Calls `tool` (as the run's list has it) of `server` with `arguments`,
    /// until it answers, its time is up or `stop` is pressed: the server's
    /// result (a `CallToolResult`).
    pub async fn call(
        &self,
        server: &McpServer,
        project: Option<&Path>,
        via: Option<&str>,
        tool: &Tool,
        arguments: Value,
        stop: &StopSignal,
    ) -> Result<Value, CallError> {
        let unavailable = |text: String| CallError {
            text,
            bad_arguments: false,
        };
        let instance = self.instance(server, project, via);
        let session = self
            .session(&instance, server, project, via, None)
            .await
            .map_err(unavailable)?;
        match session.call(tool, arguments, CALL_WITHIN, Some(stop)).await {
            Ok(result) => Ok(result),
            Err(failure) => {
                if matches!(failure, Failure::Gone(_)) {
                    down(&instance, &session).await;
                }
                Err(CallError {
                    bad_arguments: matches!(
                        failure,
                        Failure::Rpc {
                            code: rpc::INVALID_PARAMS,
                            ..
                        }
                    ),
                    text: failure.text(),
                })
            }
        }
    }

    /// Starts `server` as the page has it, with the secrets `given` (by
    /// key, `env/NAME`) over the saved ones, lists its tools, and stops it
    /// (Q04); on a daemon, a loopback URL's `via` the computer that asked.
    pub async fn test(
        &self,
        server: &McpServer,
        given: &HashMap<String, String>,
        project: Option<&Path>,
        via: Option<&str>,
    ) -> Result<Tested, String> {
        let folder = folder_for(&server.transport, project);
        let session = self
            .open(server, folder.as_deref(), via, Some(given))
            .await?;
        let listed = session.list_tools().await;
        let tested = match listed {
            Ok(mut tools) => {
                tools.sort_by(|a, b| a.name.cmp(&b.name));
                Ok(Tested {
                    server: session.server.clone(),
                    protocol: session.era.version().to_string(),
                    tools: tools
                        .into_iter()
                        .map(|t| TestedTool {
                            name: t.name,
                            description: t.description,
                            read_only: t.read_only,
                        })
                        .collect(),
                })
            }
            Err(failure) => Err(session.link.explained(failure.text())),
        };
        session.stop().await;
        tested
    }

    /// Stops every server: as the app exits, or as a daemon forgets the
    /// secrets they started with. A call going on one fails; the next use
    /// starts one again.
    pub async fn shutdown(&self) {
        let all: Vec<Arc<Instance>> = lock(&self.0.servers).drain().map(|(_, i)| i).collect();
        let stops = all.into_iter().map(|instance| async move {
            if let State::Up(session) =
                std::mem::replace(&mut *instance.state.lock().await, State::Down)
            {
                session.stop().await;
            }
        });
        futures_util::future::join_all(stops).await;
    }

    /// The server where it runs for `project`, `via` a computer: a running
    /// one with the same settings, else a new one, which replaces one with
    /// other settings.
    fn instance(&self, server: &McpServer, project: Option<&Path>, via: Option<&str>) -> Busy {
        let key = self.key(server, project, via);
        let mut running = lock(&self.0.servers);
        if let Some(instance) = running.get(&key) {
            if instance.transport == server.transport {
                return Busy::new(instance.clone());
            }
        }
        let instance = Arc::new(Instance {
            transport: server.transport.clone(),
            state: tokio::sync::Mutex::new(State::Down),
            last_used: Mutex::new(Instant::now()),
            in_use: AtomicUsize::new(0),
        });
        if let Some(old) = running.insert(key, instance.clone()) {
            stop_all(vec![old]);
        }
        Busy::new(instance)
    }

    /// The instance's session: the running one, else one started now. One
    /// that failed lately gives its error without starting again.
    async fn session(
        &self,
        instance: &Instance,
        server: &McpServer,
        project: Option<&Path>,
        via: Option<&str>,
        given: Option<&HashMap<String, String>>,
    ) -> Result<Arc<Session>, String> {
        let mut state = instance.state.lock().await;
        match &*state {
            State::Up(session) if session.link.alive() => return Ok(session.clone()),
            State::Failed { at, error } if at.elapsed() < RETRY_AFTER => return Err(error.clone()),
            _ => {}
        }
        let folder = folder_for(&server.transport, project);
        match self.open(server, folder.as_deref(), via, given).await {
            Ok(session) => {
                let session = Arc::new(session);
                *state = State::Up(session.clone());
                Ok(session)
            }
            Err(error) => {
                *state = State::Failed {
                    at: Instant::now(),
                    error: error.clone(),
                };
                Err(error)
            }
        }
    }

    /// Starts `server` in `folder` and speaks to it, a loopback URL's
    /// `via` that computer on a daemon: its env vars or headers with their
    /// secrets, `given` ones first.
    async fn open(
        &self,
        server: &McpServer,
        folder: Option<&Path>,
        via: Option<&str>,
        given: Option<&HashMap<String, String>>,
    ) -> Result<Session, String> {
        let values = self.values(server, given).await?;
        let link = match &server.transport {
            McpTransport::Command { command, args, .. } => {
                Link::Stdio(stdio::Stdio::start(command, args, &values, folder)?)
            }
            McpTransport::Url { url, .. } => {
                let relayed = self.relayed(url, via)?;
                Link::Http(http::Http::new(url, &values, relayed)?)
            }
        };
        Session::open(link).await
    }

    /// A server's env vars or headers, each secret's value from `given`
    /// or the keychain: read off the async threads, as the proxies'
    /// passwords are (net), since the keychain may take a moment or ask
    /// the user.
    async fn values(
        &self,
        server: &McpServer,
        given: Option<&HashMap<String, String>>,
    ) -> Result<Vec<(String, String)>, String> {
        let source = lock(&self.0.secrets).clone();
        let server = server.clone();
        let given = given.cloned().unwrap_or_default();
        crate::rt::spawn_blocking(move || {
            server
                .transport
                .values()
                .iter()
                .filter(|v| !v.name.trim().is_empty())
                .map(|value| {
                    if !value.secret {
                        return Ok((value.name.clone(), value.value.clone()));
                    }
                    let key = server.transport.secret_key(&value.name);
                    if let Some(given) = given.get(&key) {
                        return Ok((value.name.clone(), given.clone()));
                    }
                    let saved = match &source {
                        Some(source) => source(&server.id, &key),
                        None => Ok(None),
                    };
                    match saved {
                        Ok(Some(secret)) => Ok((value.name.clone(), secret)),
                        Ok(None) => Err(format!(
                            "{}'s value isn't saved: enter it in Settings → MCP servers.",
                            value.name
                        )),
                        Err(e) => Err(format!(
                            "{}'s value couldn't be read from the keychain: {e}",
                            value.name
                        )),
                    }
                })
                .collect()
        })
        .await
        .map_err(|e| format!("The server's secret values couldn't be read: {e}"))?
    }
}

/// Marks the instance's `session` failed with `error`, if it's still its.
async fn failed(instance: &Instance, session: &Arc<Session>, error: &str) {
    let mut state = instance.state.lock().await;
    if matches!(&*state, State::Up(current) if Arc::ptr_eq(current, session)) {
        *state = State::Failed {
            at: Instant::now(),
            error: error.to_string(),
        };
    }
}

/// Marks the instance's `session`, which ended, down, if it's still its:
/// its next use starts it again.
async fn down(instance: &Instance, session: &Arc<Session>) {
    let mut state = instance.state.lock().await;
    if matches!(&*state, State::Up(current) if Arc::ptr_eq(current, session)) {
        *state = State::Down;
    }
}

/// Stops these instances' sessions, on tasks of their own. A call still
/// going on one fails, saying the server ended.
fn stop_all(instances: Vec<Arc<Instance>>) {
    for instance in instances {
        crate::rt::spawn(async move {
            let state = std::mem::replace(&mut *instance.state.lock().await, State::Down);
            if let State::Up(session) = state {
                session.stop().await;
            }
        });
    }
}
