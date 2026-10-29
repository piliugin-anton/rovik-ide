//! Signing in to a URL server (mcp-plan-2.md, Q14), as the MCP spec's
//! Authorization describes: the server's protected resource metadata (RFC
//! 9728: its 401's `resource_metadata`, else the well-known URLs), its
//! authorization server's (RFC 8414, or OpenID Connect discovery), a client
//! (the one the user entered on the server's form, else Rovik's Client ID
//! Metadata Document, else one registered, RFC 7591), and the authorization
//! code flow with PKCE (S256) and the resource (RFC 8707), in the user's
//! browser, back to a loopback redirect here. What it gives, a grant
//! ([`McpGrant`]), goes on each request as `Authorization: Bearer`,
//! refreshed before it expires and when the server refuses it ([`Bearer`]).

use super::rpc::Failure;
use futures_util::future::BoxFuture;
use reqwest::header::{HeaderMap, ACCEPT, AUTHORIZATION, WWW_AUTHENTICATE};
use rovik_protocol::Secret;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub use rovik_protocol::mcp::McpGrant;

/// Rovik's client ID where an authorization server takes Client ID
/// Metadata Documents: the document itself, published with the site
/// (website/public/oauth/client.json). It must never move: servers cache it.
pub const CLIENT_ID_URL: &str = "https://rovik-ide.dev/oauth/client.json";

/// How long the user has to sign in, in the browser.
#[cfg(not(any(test, feature = "testing")))]
const SIGN_IN_WITHIN: Duration = Duration::from_secs(10 * 60);
#[cfg(any(test, feature = "testing"))]
const SIGN_IN_WITHIN: Duration = Duration::from_secs(10);
/// How long one request to an authorization server may take.
const ASK_WITHIN: Duration = Duration::from_secs(30);
/// An access token this close to its expiry is refreshed first.
const REFRESH_AHEAD_MS: u64 = 60_000;

/// What a server says when it needs a sign-in Rovik doesn't have.
pub const NEEDS_SIGN_IN: &str =
    "The server needs you to sign in: sign in from Settings → MCP servers.";
/// What a server whose sign-in was refused says to do.
pub(super) const SIGN_IN_AGAIN: &str = "sign in again from Settings → MCP servers";

/// Where the servers' sign-ins are kept, by server id: the app's keychain,
/// a daemon's key store. Called on a blocking thread.
pub trait GrantStore: Send + Sync {
    fn get(&self, server: &str) -> Result<Option<McpGrant>, String>;
    /// Keeps `grant`, as a refresh gave it.
    fn put(&self, server: &str, grant: &McpGrant) -> Result<(), String>;
}

/// An OAuth client Rovik signs in as, with an authorization server: the
/// one the user entered, Rovik's metadata document, or one it registered.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Client {
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<Secret>,
    /// `none`, `client_secret_basic` or `client_secret_post`.
    pub auth_method: String,
    /// The redirect it was registered with: its port is used again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_uri: Option<String>,
}

/// Where the clients Rovik registered are kept, by issuer: one is never
/// used with another authorization server.
pub trait Registrations: Send + Sync {
    fn get(&self, issuer: &str) -> Option<Client>;
    fn put(&self, issuer: &str, client: &Client) -> Result<(), String>;
}

/*
 * The server's challenge
 */

/// What a 401's or a 403's `WWW-Authenticate: Bearer …` says (RFC 6750,
/// RFC 9728).
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Challenge {
    pub resource_metadata: Option<String>,
    pub scope: Option<String>,
    pub error: Option<String>,
}

/// The Bearer challenge among `header`'s challenges, if there's one.
pub(super) fn challenge(header: &str) -> Option<Challenge> {
    let mut rest = header.trim();
    loop {
        let (scheme, after) = token(rest);
        if scheme.is_empty() {
            return None;
        }
        let (params, next) = auth_params(after);
        if scheme.eq_ignore_ascii_case("bearer") {
            let get = |name: &str| {
                (params.iter())
                    .find(|(n, _)| n.eq_ignore_ascii_case(name))
                    .map(|(_, v)| v.clone())
            };
            return Some(Challenge {
                resource_metadata: get("resource_metadata"),
                scope: get("scope"),
                error: get("error"),
            });
        }
        rest = next;
    }
}

/// The token `text` starts with, and what follows it.
fn token(text: &str) -> (&str, &str) {
    let text = text.trim_start_matches([' ', ',']);
    let end = text
        .find(|c: char| c.is_whitespace() || c == ',' || c == '=')
        .unwrap_or(text.len());
    (&text[..end], &text[end..])
}

/// A challenge's parameters, `name=value` or `name="value"`, up to the
/// next challenge's scheme; and where that starts.
fn auth_params(mut text: &str) -> (Vec<(String, String)>, &str) {
    let mut params = Vec::new();
    loop {
        let before = text;
        let (name, after) = token(text);
        let after = after.trim_start();
        let Some(value) = after.strip_prefix('=').filter(|_| !name.is_empty()) else {
            // Not a parameter: the next challenge's scheme, or the end.
            return (params, before);
        };
        let value = value.trim_start();
        let (value, after) = match value.strip_prefix('"') {
            Some(quoted) => {
                let mut out = String::new();
                let mut chars = quoted.char_indices();
                let mut end = quoted.len();
                while let Some((at, c)) = chars.next() {
                    match c {
                        '\\' => {
                            if let Some((_, escaped)) = chars.next() {
                                out.push(escaped);
                            }
                        }
                        '"' => {
                            end = at + 1;
                            break;
                        }
                        c => out.push(c),
                    }
                }
                (out, &quoted[end..])
            }
            None => {
                let end = value.find([',', ' ']).unwrap_or(value.len());
                (value[..end].to_string(), &value[end..])
            }
        };
        params.push((name.to_string(), value));
        text = after;
    }
}

/// The Bearer challenge of a refused request's answer.
pub(super) fn challenge_of(headers: &HeaderMap) -> Option<Challenge> {
    (headers.get_all(WWW_AUTHENTICATE).iter())
        .filter_map(|v| v.to_str().ok())
        .find_map(challenge)
}

/*
 * Discovery
 */

/// Where a server's sign-in goes: its resource, its authorization server
/// and that server's endpoints, and the scopes to ask for.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Discovered {
    /// The server, as its tokens are for it.
    pub resource: String,
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    /// It takes Client ID Metadata Documents.
    pub metadata_documents: bool,
    /// The ways a client may prove itself at its token endpoint.
    pub auth_methods: Vec<String>,
    /// It answers with `iss` (RFC 9207).
    pub says_issuer: bool,
    /// The scopes to ask for: the challenge's, else all the resource's.
    pub scope: Option<String>,
}

/// GETs `url`'s JSON object: none for an answer that isn't one (a 404),
/// the next place to look being the caller's.
async fn json_at(url: &str) -> Result<Option<Value>, String> {
    let client = crate::net::client().await;
    let asked = client.get(url).header(ACCEPT, "application/json").send();
    let response = tokio::time::timeout(ASK_WITHIN, asked)
        .await
        .map_err(|_| format!("{} didn't answer in time", origin_of(url)))?
        .map_err(|e| crate::net::error_text(&e.without_url()))?;
    if !response.status().is_success() {
        return Ok(None);
    }
    let body = response
        .bytes()
        .await
        .map_err(|e| crate::net::error_text(&e.without_url()))?;
    Ok(serde_json::from_slice::<Value>(&body)
        .ok()
        .filter(Value::is_object))
}

/// A URL's origin, for a message: never its path or query, where a token
/// may be.
fn origin_of(url: &str) -> String {
    reqwest::Url::parse(url).map_or_else(
        |_| "the server".into(),
        |u| u.origin().ascii_serialization(),
    )
}

/// `url` as a resource is written (RFC 8707): no query or fragment, no
/// trailing slash on its path.
fn canonical(url: &reqwest::Url) -> String {
    let mut url = url.clone();
    url.set_query(None);
    url.set_fragment(None);
    let text = url.to_string();
    text.strip_suffix('/').map_or(text.clone(), str::to_string)
}

/// Whether `resource` covers `server`: the same origin, its path the
/// server's or one above it.
fn covers(resource: &reqwest::Url, server: &reqwest::Url) -> bool {
    let segments = |u: &reqwest::Url| -> Vec<String> {
        u.path()
            .split('/')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    };
    let (theirs, ours) = (segments(resource), segments(server));
    resource.origin() == server.origin() && ours.starts_with(&theirs)
}

/// The well-known URLs of `url`'s `suffix` metadata: with `url`'s path
/// after the suffix, then without it (RFC 8414, RFC 9728).
fn well_known(url: &reqwest::Url, suffix: &str) -> Vec<String> {
    let origin = url.origin().ascii_serialization();
    let path = url.path().trim_end_matches('/');
    let mut urls = Vec::new();
    if !path.is_empty() {
        urls.push(format!("{origin}/.well-known/{suffix}{path}"));
    }
    urls.push(format!("{origin}/.well-known/{suffix}"));
    urls
}

/// The sign-in of the server at `url`, as its challenge (if any) and its
/// metadata say.
pub(super) async fn discover(
    url: &str,
    challenge: Option<&Challenge>,
) -> Result<Discovered, String> {
    let server = reqwest::Url::parse(url).map_err(|e| format!("The URL can't be used: {e}."))?;
    let places = match challenge.and_then(|c| c.resource_metadata.clone()) {
        Some(named) => vec![named],
        None => well_known(&server, "oauth-protected-resource"),
    };
    let mut found = None;
    for place in places {
        if let Some(metadata) = json_at(&place).await? {
            found = Some(metadata);
            break;
        }
    }
    let Some(resource_metadata) = found else {
        return Err(
            "The server asks for a sign-in, but doesn't say where (it has no protected resource \
             metadata)."
                .into(),
        );
    };
    let resource = match resource_metadata.get("resource").and_then(Value::as_str) {
        Some(named) => {
            let named = reqwest::Url::parse(named).map_err(|_| {
                format!("The server's metadata names a resource Rovik can't read: {named}")
            })?;
            if !covers(&named, &server) {
                return Err(format!(
                    "The server's sign-in is for another resource ({}): Rovik doesn't send its \
                     tokens there.",
                    named.origin().ascii_serialization()
                ));
            }
            canonical(&named)
        }
        None => canonical(&server),
    };
    let issuer = (resource_metadata.get("authorization_servers"))
        .and_then(Value::as_array)
        .and_then(|servers| servers.first())
        .and_then(Value::as_str)
        .ok_or("The server's metadata names no authorization server to sign in with.")?
        .to_string();
    let issuer_url = reqwest::Url::parse(&issuer)
        .map_err(|_| format!("The server's authorization server isn't a URL: {issuer}"))?;
    if issuer_url.scheme() != "https" && !super::on_loopback(&issuer) {
        return Err(format!(
            "The server's authorization server isn't on HTTPS ({}): Rovik doesn't sign in there.",
            issuer_url.origin().ascii_serialization()
        ));
    }
    let metadata = authorization_server(&issuer_url).await?;
    let text = |key: &str| {
        metadata
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let list = |key: &str| -> Option<Vec<String>> {
        let list = metadata.get(key)?.as_array()?;
        Some(
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
        )
    };
    let said = text("issuer").unwrap_or_default();
    if said.trim_end_matches('/') != issuer.trim_end_matches('/') {
        return Err(format!(
            "The authorization server's metadata is for another issuer ({said}): Rovik doesn't \
             sign in with it."
        ));
    }
    // PKCE with S256: a server that says which it takes must take it.
    if list("code_challenge_methods_supported").is_some_and(|m| !m.iter().any(|m| m == "S256")) {
        return Err(
            "The server's sign-in doesn't take PKCE with S256, which Rovik needs to sign in \
             safely."
                .into(),
        );
    }
    let endpoint = |key: &str| text(key).ok_or(format!("The authorization server has no {key}."));
    let scopes_supported = list_of(&resource_metadata, "scopes_supported");
    let scope = (challenge.and_then(|c| c.scope.clone()))
        .or_else(|| (!scopes_supported.is_empty()).then(|| scopes_supported.join(" ")));
    Ok(Discovered {
        resource,
        issuer,
        authorization_endpoint: endpoint("authorization_endpoint")?,
        token_endpoint: endpoint("token_endpoint")?,
        registration_endpoint: text("registration_endpoint"),
        metadata_documents: metadata.get("client_id_metadata_document_supported")
            == Some(&Value::Bool(true)),
        auth_methods: list("token_endpoint_auth_methods_supported").unwrap_or_default(),
        says_issuer: metadata.get("authorization_response_iss_parameter_supported")
            == Some(&Value::Bool(true)),
        scope,
    })
}

/// A JSON list of strings, as `object[key]` has it.
fn list_of(object: &Value, key: &str) -> Vec<String> {
    (object
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten())
    .filter_map(Value::as_str)
    .map(str::to_string)
    .collect()
}

/// The metadata of the authorization server `issuer`: RFC 8414's, then
/// OpenID Connect's, a path inserted after the well-known part and then
/// appended to the issuer, as the spec has clients look.
async fn authorization_server(issuer: &reqwest::Url) -> Result<Value, String> {
    let origin = issuer.origin().ascii_serialization();
    let places: Vec<String> = match issuer.path().trim_end_matches('/') {
        "" => vec![
            format!("{origin}/.well-known/oauth-authorization-server"),
            format!("{origin}/.well-known/openid-configuration"),
        ],
        path => vec![
            format!("{origin}/.well-known/oauth-authorization-server{path}"),
            format!("{origin}/.well-known/openid-configuration{path}"),
            format!("{origin}{path}/.well-known/openid-configuration"),
        ],
    };
    for place in places {
        if let Some(metadata) = json_at(&place).await? {
            return Ok(metadata);
        }
    }
    Err(format!(
        "The authorization server {} publishes no metadata: Rovik can't sign in with it.",
        issuer.origin().ascii_serialization()
    ))
}

/*
 * A sign-in, whole
 */

/// Signs in to the URL server `url`, its requests carrying `headers` (the
/// settings', secrets filled in): its 401 asked for (a handshake's POST,
/// or the event stream's GET on HTTP+SSE), its sign-in discovered, and the
/// user sent through it in the browser with `open`, as the client they
/// `entered` or Rovik's (Q14). Registrations Rovik makes are kept in
/// `registrations`. Its grant, for the place this runs.
pub async fn sign_in_to(
    url: &str,
    sse: bool,
    headers: &[(String, String)],
    entered: Option<(String, Option<String>)>,
    registrations: Arc<dyn Registrations>,
    open: &(dyn Fn(&str) -> Result<(), String> + Send + Sync),
) -> Result<McpGrant, String> {
    let url = super::http::checked_url(url)?.to_string();
    let challenge = probe(&url, sse, headers).await?;
    let found = discover(&url, challenge.as_ref()).await?;
    let kept = registered(&registrations, &found.issuer).await;
    let kept_port = (kept.and_then(|kept| kept.redirect_uri))
        .and_then(|uri| reqwest::Url::parse(&uri).ok())
        .and_then(|uri| uri.port());
    let redirect = Redirect::listen(kept_port).await?;
    let client = client(&found, entered, &registrations, &redirect.uri).await?;
    sign_in(&found, &client, &redirect, open).await
}

/// The client registered with `issuer`, read off the async threads: the
/// keychain may take a moment, or ask the user.
async fn registered(registrations: &Arc<dyn Registrations>, issuer: &str) -> Option<Client> {
    let (registrations, issuer) = (registrations.clone(), issuer.to_string());
    let read = crate::rt::spawn_blocking(move || registrations.get(&issuer)).await;
    read.ok().flatten()
}

/// What the server answers a request without a sign-in: its challenge
/// when it refuses it (401); none when it takes it, its metadata then
/// looked for where it's well known.
async fn probe(
    url: &str,
    sse: bool,
    headers: &[(String, String)],
) -> Result<Option<Challenge>, String> {
    let client = crate::net::client().await;
    let mut request = match sse {
        true => client.get(url).header(ACCEPT, "text/event-stream"),
        false => {
            let hello = super::rpc::request(
                1,
                "initialize",
                serde_json::json!({
                    "protocolVersion": super::rpc::LEGACY,
                    "capabilities": {},
                    "clientInfo": super::rpc::client_info(),
                }),
            );
            (client.post(url))
                .header(ACCEPT, "application/json, text/event-stream")
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(hello.to_string())
        }
    };
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_str());
    }
    let response = tokio::time::timeout(ASK_WITHIN, request.send())
        .await
        .map_err(|_| "The server didn't answer in time.".to_string())?
        .map_err(|e| crate::net::error_text(&e.without_url()))?;
    Ok(match response.status().as_u16() {
        401 => challenge_of(response.headers()),
        _ => None,
    })
}

/*
 * The client
 */

/// The client Rovik signs in as with `found`'s authorization server, its
/// redirect back to `redirect_uri`: the one the user `entered` (with its
/// secret), else its metadata document where the server takes those, else
/// the one registered with it before for this redirect, else one it
/// registers now (Dynamic Client Registration, kept by issuer).
pub(super) async fn client(
    found: &Discovered,
    entered: Option<(String, Option<String>)>,
    registrations: &Arc<dyn Registrations>,
    redirect_uri: &str,
) -> Result<Client, String> {
    if let Some((client_id, secret)) = entered {
        let auth_method = match &secret {
            None => "none",
            Some(_)
                if found.auth_methods.iter().any(|m| m == "client_secret_post")
                    && !found
                        .auth_methods
                        .iter()
                        .any(|m| m == "client_secret_basic") =>
            {
                "client_secret_post"
            }
            Some(_) => "client_secret_basic",
        };
        return Ok(Client {
            client_id,
            client_secret: secret.map(Secret),
            auth_method: auth_method.into(),
            redirect_uri: None,
        });
    }
    if found.metadata_documents {
        return Ok(Client {
            client_id: CLIENT_ID_URL.into(),
            client_secret: None,
            auth_method: "none".into(),
            redirect_uri: None,
        });
    }
    if let Some(kept) = registered(registrations, &found.issuer).await {
        if kept.redirect_uri.as_deref() == Some(redirect_uri) {
            return Ok(kept);
        }
    }
    let Some(endpoint) = &found.registration_endpoint else {
        return Err(format!(
            "The server's sign-in needs an OAuth client ID: register one with {} and enter it on \
             the server's form.",
            origin_of(&found.issuer)
        ));
    };
    let made = register(endpoint, redirect_uri).await?;
    let (to, issuer, kept) = (registrations.clone(), found.issuer.clone(), made.clone());
    let put = crate::rt::spawn_blocking(move || to.put(&issuer, &kept)).await;
    put.map_err(|e| e.to_string())
        .and_then(|put| put)
        .map_err(|e| {
            format!("Rovik's registration with the authorization server wasn't kept: {e}")
        })?;
    Ok(made)
}

/// Registers Rovik with `endpoint` (RFC 7591) as a native app whose
/// redirect is `redirect_uri`.
async fn register(endpoint: &str, redirect_uri: &str) -> Result<Client, String> {
    let asked = serde_json::json!({
        "client_name": "Rovik IDE",
        "client_uri": "https://rovik-ide.dev",
        "logo_uri": "https://rovik-ide.dev/apple-touch-icon.png",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
        "application_type": "native",
    });
    let client = crate::net::client().await;
    let posted = client.post(endpoint).json(&asked).send();
    let response = tokio::time::timeout(ASK_WITHIN, posted)
        .await
        .map_err(|_| "The authorization server didn't register Rovik in time.".to_string())?
        .map_err(|e| crate::net::error_text(&e.without_url()))?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "The authorization server didn't register Rovik ({}): enter an OAuth client ID on the \
             server's form.",
            oauth_error(&body).unwrap_or_else(|| format!("HTTP {}", status.as_u16()))
        ));
    }
    let client_id = (body.get("client_id").and_then(Value::as_str))
        .ok_or("The authorization server registered Rovik without a client ID.")?;
    let client_secret =
        (body.get("client_secret").and_then(Value::as_str)).map(|s| Secret(s.into()));
    let auth_method = match body
        .get("token_endpoint_auth_method")
        .and_then(Value::as_str)
    {
        Some(method) => method.to_string(),
        None if client_secret.is_some() => "client_secret_basic".into(),
        None => "none".into(),
    };
    Ok(Client {
        client_id: client_id.to_string(),
        client_secret,
        auth_method,
        redirect_uri: Some(redirect_uri.to_string()),
    })
}

/// An OAuth error's text: its `error`, and its description.
fn oauth_error(body: &Value) -> Option<String> {
    let error = body.get("error").and_then(Value::as_str)?;
    Some(
        match body.get("error_description").and_then(Value::as_str) {
            Some(description) => format!("{error}: {description}"),
            None => error.to_string(),
        },
    )
}

/*
 * Signing in
 */

/// A PKCE verifier (RFC 7636): 32 random bytes, as base64url.
fn verifier() -> String {
    use base64::Engine;
    let bytes: [u8; 32] = rand::random();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The S256 challenge of `verifier`.
fn code_challenge(verifier: &str) -> String {
    use base64::Engine;
    use sha2::Digest;
    let hash = sha2::Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash)
}

/// How the user's browser comes back: a loopback listener's port, and its
/// redirect URI there.
pub(super) struct Redirect {
    listener: tokio::net::TcpListener,
    pub uri: String,
}

impl Redirect {
    /// Listens on the loopback: on `port` when it's free (a registration's,
    /// whose redirect is that port's), else on any.
    pub(super) async fn listen(port: Option<u16>) -> Result<Redirect, String> {
        let bound = match port {
            Some(port) => tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .ok(),
            None => None,
        };
        let listener = match bound {
            Some(listener) => listener,
            None => tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .map_err(|e| format!("Rovik couldn't wait for the sign-in here: {e}"))?,
        };
        let port = listener
            .local_addr()
            .map_err(|e| format!("Rovik couldn't wait for the sign-in here: {e}"))?
            .port();
        Ok(Redirect {
            listener,
            uri: format!("http://127.0.0.1:{port}/callback"),
        })
    }

    /// The callback's query, once the browser comes back to it; anything
    /// else asked of the listener is answered 404.
    async fn callback(&self) -> Result<Vec<(String, String)>, String> {
        loop {
            let (mut socket, _) = self
                .listener
                .accept()
                .await
                .map_err(|e| format!("The sign-in's answer couldn't be read: {e}"))?;
            let mut head = Vec::new();
            let mut buf = [0u8; 4096];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 64 << 10 {
                match socket.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => head.extend_from_slice(&buf[..n]),
                }
            }
            let line = String::from_utf8_lossy(&head);
            let target = line.split_whitespace().nth(1).unwrap_or("").to_string();
            let Some(query) = target.strip_prefix("/callback") else {
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await;
                continue;
            };
            let query = query.strip_prefix('?').unwrap_or("");
            let pairs: Vec<(String, String)> = (url_pairs(query).into_iter()).collect();
            let page = match pairs.iter().any(|(k, _)| k == "error") {
                true => "<p>The sign-in didn't go through. Rovik says why.</p>",
                false => "<p>Signed in. You can close this tab and go back to Rovik.</p>",
            };
            let html = format!(
                "<!doctype html><meta charset=utf-8><title>Rovik IDE</title><body style=\"font-family:sans-serif\">{page}</body>"
            );
            let answer = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
                html.len()
            );
            let _ = socket.write_all(answer.as_bytes()).await;
            return Ok(pairs);
        }
    }
}

/// A query's pairs, decoded.
fn url_pairs(query: &str) -> Vec<(String, String)> {
    let url = reqwest::Url::parse(&format!("http://x/?{query}"));
    url.map(|u| u.query_pairs().into_owned().collect())
        .unwrap_or_default()
}

/// Signs in with `found`'s authorization server as `client`, the user
/// sent there with `open` (the browser) and coming back to `redirect`:
/// the authorization code flow with PKCE and the resource. Its grant.
pub(super) async fn sign_in(
    found: &Discovered,
    client: &Client,
    redirect: &Redirect,
    open: &(dyn Fn(&str) -> Result<(), String> + Send + Sync),
) -> Result<McpGrant, String> {
    let verifier = verifier();
    // As random as the verifier: no other page's answer can pass for it.
    let state = self::verifier();
    let mut url = reqwest::Url::parse(&found.authorization_endpoint)
        .map_err(|_| "The authorization server's sign-in page isn't a URL.".to_string())?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", &client.client_id)
            .append_pair("redirect_uri", &redirect.uri)
            .append_pair("code_challenge", &code_challenge(&verifier))
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("resource", &found.resource);
        if let Some(scope) = &found.scope {
            query.append_pair("scope", scope);
        }
    }
    open(url.as_str())?;
    let answered = tokio::time::timeout(SIGN_IN_WITHIN, redirect.callback())
        .await
        .map_err(|_| {
            "The sign-in wasn't finished in the browser in time: sign in again.".to_string()
        })??;
    let get = |key: &str| {
        (answered.iter())
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    if let Some(error) = get("error") {
        let description = get("error_description")
            .map(|d| format!(": {d}"))
            .unwrap_or_default();
        return Err(format!("The sign-in was refused ({error}{description})."));
    }
    if get("state").as_deref() != Some(state.as_str()) {
        return Err("The sign-in's answer wasn't for this sign-in: sign in again.".into());
    }
    // RFC 9207: an answer from another issuer is a mix-up.
    match get("iss") {
        Some(iss) if iss.trim_end_matches('/') != found.issuer.trim_end_matches('/') => {
            return Err(format!(
                "The sign-in's answer came from another authorization server ({iss}): Rovik \
                 doesn't take it."
            ))
        }
        None if found.says_issuer => {
            return Err(
                "The sign-in's answer didn't say its issuer, as its server said it would.".into(),
            )
        }
        _ => {}
    }
    let code = get("code").ok_or("The sign-in's answer has no code.")?;
    let mut form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code),
        ("redirect_uri", redirect.uri.clone()),
        ("code_verifier", verifier),
        ("resource", found.resource.clone()),
    ];
    let scope = found.scope.clone().unwrap_or_default();
    let base = McpGrant {
        issuer: found.issuer.clone(),
        token_endpoint: found.token_endpoint.clone(),
        client_id: client.client_id.clone(),
        client_secret: client.client_secret.clone(),
        auth_method: client.auth_method.clone(),
        resource: found.resource.clone(),
        scope,
        access_token: Secret(String::new()),
        refresh_token: None,
        expires_at: None,
    };
    let tokens = token_request(&base, &mut form)
        .await
        .map_err(|failed| match failed {
            Refusal::Refused(why) | Refusal::Failed(why) => {
                format!("The sign-in's tokens weren't given: {why}")
            }
        })?;
    Ok(granted(base, tokens))
}

/// Why a token request gave no tokens.
#[derive(Debug, PartialEq)]
pub(super) enum Refusal {
    /// The authorization server refused the grant: signing in again is all
    /// that's left.
    Refused(String),
    /// It couldn't be asked, or its answer couldn't be read: trying again
    /// later may do.
    Failed(String),
}

/// Asks the token endpoint of `grant` with `form`, as its client: the
/// tokens it answers with.
async fn token_request(
    grant: &McpGrant,
    form: &mut Vec<(&'static str, String)>,
) -> Result<Value, Refusal> {
    let client = crate::net::client().await;
    let mut request = client
        .post(&grant.token_endpoint)
        .header(ACCEPT, "application/json");
    match (grant.auth_method.as_str(), &grant.client_secret) {
        ("client_secret_basic", Some(secret)) => {
            use base64::Engine;
            // RFC 6749 2.3.1: each form-encoded, then the pair base64.
            let encoded = |s: &str| form_body(&[("", s.to_string())]).split_off(1);
            let pair = format!("{}:{}", encoded(&grant.client_id), encoded(&secret.0));
            let basic = base64::engine::general_purpose::STANDARD.encode(pair);
            request = request.header(AUTHORIZATION, format!("Basic {basic}"));
            form.push(("client_id", grant.client_id.clone()));
        }
        ("client_secret_post", Some(secret)) => {
            form.push(("client_id", grant.client_id.clone()));
            form.push(("client_secret", secret.0.clone()));
        }
        _ => form.push(("client_id", grant.client_id.clone())),
    }
    let body = form_body(form);
    let sent = request
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send();
    let response = tokio::time::timeout(ASK_WITHIN, sent)
        .await
        .map_err(|_| Refusal::Failed("the authorization server didn't answer in time".into()))?
        .map_err(|e| Refusal::Failed(crate::net::error_text(&e.without_url())))?;
    let status = response.status();
    let answer: Value = response.json().await.unwrap_or_default();
    if status.is_success() && answer.get("access_token").and_then(Value::as_str).is_some() {
        return Ok(answer);
    }
    let why = oauth_error(&answer).unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
    match status.as_u16() {
        400 | 401 => Err(Refusal::Refused(why)),
        _ => Err(Refusal::Failed(why)),
    }
}

/// `pairs` as a form's body (`application/x-www-form-urlencoded`).
fn form_body(pairs: &[(&str, String)]) -> String {
    let mut url = reqwest::Url::parse("http://form/").expect("a constant URL");
    url.query_pairs_mut()
        .extend_pairs(pairs.iter().map(|(k, v)| (*k, v.as_str())));
    url.query().unwrap_or_default().to_string()
}

/// `grant` with the tokens of `answer`: its access token, its refresh
/// token (a new one rotates the old one out), when it expires, its scope.
fn granted(mut grant: McpGrant, answer: Value) -> McpGrant {
    let text = |key: &str| answer.get(key).and_then(Value::as_str).map(str::to_string);
    grant.access_token = Secret(text("access_token").unwrap_or_default());
    if let Some(refresh) = text("refresh_token") {
        grant.refresh_token = Some(Secret(refresh));
    }
    grant.expires_at =
        (answer.get("expires_in").and_then(Value::as_u64)).map(|seconds| now_ms() + seconds * 1000);
    if let Some(scope) = text("scope") {
        grant.scope = scope;
    }
    grant
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// `grant` refreshed: its new tokens; or why not.
pub(super) async fn refresh(grant: &McpGrant) -> Result<McpGrant, Refusal> {
    let Some(refresh_token) = &grant.refresh_token else {
        return Err(Refusal::Refused("it has no refresh token".into()));
    };
    let mut form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.0.clone()),
        ("resource", grant.resource.clone()),
    ];
    let answer = token_request(grant, &mut form).await?;
    Ok(granted(grant.clone(), answer))
}

/*
 * The token on each request
 */

/// A server's sign-in, as its requests carry it: the access token now,
/// and a fresh one when the server refuses it, kept where the grant is.
/// One for each server, so two instances never refresh it apart (its
/// refresh token rotates).
pub(super) struct Bearer {
    server: String,
    store: Arc<dyn GrantStore>,
    grant: tokio::sync::Mutex<McpGrant>,
}

impl Bearer {
    pub(super) fn new(server: &str, store: Arc<dyn GrantStore>, grant: McpGrant) -> Bearer {
        Bearer {
            server: server.to_string(),
            store,
            grant: tokio::sync::Mutex::new(grant),
        }
    }

    /// The access token to send, refreshed first when it's about to expire.
    pub(super) fn token(&self) -> BoxFuture<'_, Result<String, Failure>> {
        Box::pin(async move {
            let mut grant = self.grant.lock().await;
            let expiring = grant
                .expires_at
                .is_some_and(|at| at <= now_ms() + REFRESH_AHEAD_MS);
            if expiring && grant.refresh_token.is_some() {
                self.refreshed(&mut grant).await?;
            }
            Ok(grant.access_token.0.clone())
        })
    }

    /// A new access token, `sent` refused (a 401): refreshed, unless that
    /// happened since it was sent.
    pub(super) fn refused(&self, sent: &str) -> BoxFuture<'_, Result<String, Failure>> {
        let sent = sent.to_string();
        Box::pin(async move {
            let mut grant = self.grant.lock().await;
            if grant.access_token.0 == sent {
                self.refreshed(&mut grant).await?;
            }
            Ok(grant.access_token.0.clone())
        })
    }

    /// Refreshes `grant`, keeping what it gave.
    async fn refreshed(&self, grant: &mut McpGrant) -> Result<(), Failure> {
        match refresh(grant).await {
            Ok(fresh) => {
                *grant = fresh;
                let (store, server, kept) =
                    (self.store.clone(), self.server.clone(), grant.clone());
                let put = crate::rt::spawn_blocking(move || store.put(&server, &kept)).await;
                if let Ok(Err(e)) | Err(e) = put.map_err(|e| e.to_string()) {
                    log::warn!("an MCP server's refreshed sign-in wasn't kept: {e}");
                }
                Ok(())
            }
            Err(Refusal::Refused(why)) => Err(Failure::Gone(format!(
                "The server's sign-in was refused ({why}): {SIGN_IN_AGAIN}."
            ))),
            Err(Refusal::Failed(why)) => Err(Failure::Gone(format!(
                "The server's sign-in couldn't be refreshed: {why}"
            ))),
        }
    }
}

/// What a 401 or 403 answer to a request with a sign-in's token says: the
/// token was refused, or (403 `insufficient_scope`) it needs more scopes.
pub(super) fn wider(challenge: Option<&Challenge>) -> Option<String> {
    let challenge = challenge?;
    (challenge.error.as_deref() == Some("insufficient_scope")).then(|| {
        let scope = challenge
            .scope
            .as_deref()
            .map(|s| format!(" ({s})"))
            .unwrap_or_default();
        format!("The server needs a sign-in with more permissions{scope}: {SIGN_IN_AGAIN}.")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{OAuthOptions, OAuthServer};
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Registrations kept in memory, as the keychain keeps them.
    #[derive(Default)]
    pub(crate) struct Kept(pub Mutex<HashMap<String, Client>>);

    impl Registrations for Kept {
        fn get(&self, issuer: &str) -> Option<Client> {
            self.0.lock().unwrap().get(issuer).cloned()
        }
        fn put(&self, issuer: &str, client: &Client) -> Result<(), String> {
            self.0.lock().unwrap().insert(issuer.into(), client.clone());
            Ok(())
        }
    }

    /// A sign-in to `server`'s MCP server, its browser `server`'s own,
    /// what the sign-in page was asked kept in `asked`.
    async fn signed_in(
        server: &OAuthServer,
        entered: Option<(String, Option<String>)>,
        kept: &Arc<Kept>,
        asked: &Arc<Mutex<Vec<(String, String)>>>,
    ) -> Result<McpGrant, String> {
        let browser = |url: &str| -> Result<(), String> {
            *asked.lock().unwrap() = server.browse(url)?;
            Ok(())
        };
        let registrations: Arc<dyn Registrations> = kept.clone();
        sign_in_to(
            &server.mcp_url,
            false,
            &[],
            entered,
            registrations,
            &browser,
        )
        .await
    }

    fn asked_for<'a>(asked: &'a [(String, String)], key: &str) -> &'a str {
        (asked.iter())
            .find(|(k, _)| k == key)
            .map_or("", |(_, v)| v.as_str())
    }

    /// RFC 6750's challenges, as servers write them: quoted or not, with
    /// another scheme's after.
    #[test]
    fn a_bearer_challenge_is_read_among_others() {
        let header = r#"Basic realm="x", Bearer resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource", scope="read write", error=invalid_token"#;
        assert_eq!(
            challenge(header),
            Some(Challenge {
                resource_metadata: Some(
                    "https://mcp.example.com/.well-known/oauth-protected-resource".into()
                ),
                scope: Some("read write".into()),
                error: Some("invalid_token".into()),
            })
        );
        let escaped = challenge(r#"bearer error="insufficient_scope", scope="a \"b\"""#).unwrap();
        assert_eq!(escaped.scope.as_deref(), Some(r#"a "b""#));
        assert_eq!(challenge("Basic realm=\"x\""), None);
        assert_eq!(challenge("Bearer"), Some(Challenge::default()));
    }

    /// Q14: a server's 401 names its protected resource metadata, which
    /// names its authorization server; Rovik registers itself there (once,
    /// kept by issuer), sends the browser to sign in with PKCE (S256), the
    /// resource and the 401's scope, and takes the code back on its
    /// loopback for the tokens, the issuer checked.
    #[tokio::test]
    async fn a_sign_in_registers_rovik_and_takes_the_code_back_for_its_tokens() {
        let server = OAuthServer::start(OAuthOptions::default()).await;
        let (kept, asked) = (Arc::new(Kept::default()), Arc::default());
        let grant = signed_in(&server, None, &kept, &asked).await.unwrap();
        assert_eq!(grant.issuer, server.issuer);
        assert_eq!(grant.resource, server.mcp_url);
        assert_eq!(grant.scope, "mcp:read", "the 401's scope");
        assert!(grant.access_token.0.starts_with("at-"));
        assert!(grant.refresh_token.is_some());
        assert!(grant.expires_at.is_some_and(|at| at > now_ms()));
        let asked = asked.lock().unwrap().clone();
        assert_eq!(asked_for(&asked, "code_challenge_method"), "S256");
        assert_eq!(asked_for(&asked, "resource"), server.mcp_url);
        assert!(asked_for(&asked, "client_id").starts_with("registered-"));
        assert!(asked_for(&asked, "redirect_uri").starts_with("http://127.0.0.1:"));
        assert_eq!(grant.client_id, asked_for(&asked, "client_id"));
        assert!(kept.get(&server.issuer).is_some(), "kept by issuer");

        // Again: the registration kept is used, on the port it named.
        let again = signed_in(&server, None, &kept, &Arc::default())
            .await
            .unwrap();
        assert_eq!(again.client_id, grant.client_id);
        let registrations = server
            .seen()
            .into_iter()
            .filter(|r| r.path == "/auth/register");
        assert_eq!(registrations.count(), 1);
    }

    /// Q14: where the authorization server takes Client ID Metadata
    /// Documents, Rovik's is its client ID, and nothing is registered.
    #[tokio::test]
    async fn rovik_s_metadata_document_is_its_client_id_where_it_s_taken() {
        let options = OAuthOptions {
            metadata_documents: true,
            ..OAuthOptions::default()
        };
        let server = OAuthServer::start(options).await;
        let asked = Arc::default();
        let grant = signed_in(&server, None, &Arc::default(), &asked)
            .await
            .unwrap();
        assert_eq!(grant.client_id, CLIENT_ID_URL);
        assert!(server.seen().iter().all(|r| r.path != "/auth/register"));
    }

    /// Q14: a client the user registered and entered is used, its secret
    /// proving it at the token endpoint, and kept with the grant for its
    /// refreshes.
    #[tokio::test]
    async fn a_client_the_user_entered_signs_in_with_its_secret() {
        let options = OAuthOptions {
            registration: false,
            client: Some(("mine".into(), "s3cret".into())),
            ..OAuthOptions::default()
        };
        let server = OAuthServer::start(options).await;
        let entered = Some(("mine".to_string(), Some("s3cret".to_string())));
        let grant = signed_in(&server, entered, &Arc::default(), &Arc::default())
            .await
            .unwrap();
        assert_eq!(grant.client_id, "mine");
        assert_eq!(grant.auth_method, "client_secret_basic");
        assert_eq!(
            grant.client_secret.as_ref().map(|s| s.0.as_str()),
            Some("s3cret")
        );
        let token = server
            .seen()
            .into_iter()
            .find(|r| r.path == "/auth/token")
            .unwrap();
        assert!(token.authorization.unwrap().starts_with("Basic "));
        assert!(!token.body.contains("s3cret"), "not in the form too");

        // Without one entered, a server that registers no one says what to do.
        let none = signed_in(&server, None, &Arc::default(), &Arc::default()).await;
        let none = none.unwrap_err();
        assert!(none.contains("enter it on the server's form"), "{none}");
    }

    /// RFC 9207: an answer that names another issuer is a mix-up, and no
    /// token is asked for with it.
    #[tokio::test]
    async fn an_answer_from_another_issuer_is_refused() {
        let options = OAuthOptions {
            iss: Some("https://evil.example".into()),
            ..OAuthOptions::default()
        };
        let server = OAuthServer::start(options).await;
        let refused = signed_in(&server, None, &Arc::default(), &Arc::default()).await;
        let refused = refused.unwrap_err();
        assert!(
            refused.contains("another authorization server"),
            "{refused}"
        );
        assert!(server.seen().iter().all(|r| r.path != "/auth/token"));
    }

    /// A refresh rotates the refresh token; one the server no longer takes
    /// is refused, which only a new sign-in settles.
    #[tokio::test]
    async fn a_refresh_rotates_its_token_and_a_refused_one_says_so() {
        let server = OAuthServer::start(OAuthOptions::default()).await;
        let grant = signed_in(&server, None, &Arc::default(), &Arc::default())
            .await
            .unwrap();
        let fresh = refresh(&grant).await.unwrap();
        assert_ne!(fresh.access_token, grant.access_token);
        assert_ne!(fresh.refresh_token, grant.refresh_token, "rotated");
        let reused = refresh(&grant).await.unwrap_err();
        assert!(
            matches!(reused, Refusal::Refused(ref why) if why.contains("invalid_grant")),
            "{reused:?}"
        );
    }

    /// The metadata document the site publishes (website/public/oauth/
    /// client.json) is the client Rovik signs in as: its own URL its ID, a
    /// public client that refreshes, its redirect the loopback callback on
    /// any port, as RFC 8252 has a native app's.
    #[tokio::test]
    async fn the_site_publishes_the_client_rovik_signs_in_as() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../website/public/oauth/client.json"
        );
        let document: Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(document["client_id"], CLIENT_ID_URL);
        assert_eq!(document["token_endpoint_auth_method"], "none");
        assert_eq!(
            document["grant_types"],
            serde_json::json!(["authorization_code", "refresh_token"])
        );
        let mut redirect = reqwest::Url::parse(&Redirect::listen(None).await.unwrap().uri).unwrap();
        redirect.set_port(None).unwrap();
        assert_eq!(
            document["redirect_uris"],
            serde_json::json!([redirect.as_str()])
        );
    }

    /// The body of a token request is a form, its values encoded.
    #[test]
    fn a_form_s_values_are_encoded() {
        assert_eq!(
            form_body(&[
                ("a", "b c&d".into()),
                ("redirect_uri", "http://127.0.0.1:1/callback".into())
            ]),
            "a=b+c%26d&redirect_uri=http%3A%2F%2F127.0.0.1%3A1%2Fcallback"
        );
    }
}
