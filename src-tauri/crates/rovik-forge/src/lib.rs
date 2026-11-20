//! Global GitHub and GitLab device authorization for desktop and hosted clients.
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};
use uuid::Uuid;

/// Repository service owning an account.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// A GitHub.com account.
    Github,
    /// A GitLab.com account.
    Gitlab,
}
impl Provider {
    /// Stable provider namespace.
    pub fn name(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Gitlab => "gitlab",
        }
    }
    /// Official device verification host.
    pub fn host(self) -> &'static str {
        match self {
            Self::Github => "github.com",
            Self::Gitlab => "gitlab.com",
        }
    }
    /// Public application ID registered by the distributor or service operator.
    pub fn client_id(self) -> Result<String, String> {
        let key = match self {
            Self::Github => "ROVIK_GITHUB_CLIENT_ID",
            Self::Gitlab => "ROVIK_GITLAB_CLIENT_ID",
        };
        std::env::var(key).or_else(|_| {
            match self {
                Self::Github => option_env!("ROVIK_GITHUB_CLIENT_ID"),
                Self::Gitlab => option_env!("ROVIK_GITLAB_CLIENT_ID"),
            }
            .map(str::to_string)
            .ok_or_else(|| format!("Configure {key} for {} device sign-in.", self.host()))
        })
    }
    /// Official endpoint used to redeem or refresh OAuth tokens.
    pub fn token_url(self) -> &'static str {
        match self {
            Self::Github => "https://github.com/login/oauth/access_token",
            Self::Gitlab => "https://gitlab.com/oauth/token",
        }
    }
}

/// Displayable fields for an in-progress device authorization.
#[derive(Serialize, Deserialize)]
pub struct Prompt {
    /// Opaque identifier for polling this sign-in.
    pub challenge: Uuid,
    /// Short code the user enters at the provider.
    pub code: String,
    /// Official HTTPS verification URL.
    pub url: String,
    /// Minimum polling interval in seconds.
    pub interval: u64,
}

/// OAuth credential and the verified provider identity. Never send this to a renderer.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Authorized {
    /// Provider username at authentication time.
    pub login: String,
    /// Immutable numeric provider account identifier.
    pub account: i64,
    /// Provider credential for backend API calls.
    pub access_token: String,
    /// Refresh grant when the provider issues expiring tokens.
    pub refresh_token: Option<String>,
    /// Provider-reported token lifetime in seconds.
    pub expires_in: Option<u64>,
    /// Scopes granted by the provider.
    pub scope: Option<String>,
    /// Registered application used for refresh requests.
    pub client_id: String,
}

struct Pending {
    provider: Provider,
    code: String,
    expires: Instant,
    next: Instant,
    interval: Duration,
}
/// Pending device codes stay in this process and expire automatically.
#[derive(Default)]
pub struct SignIns(Mutex<HashMap<Uuid, Pending>>);
impl SignIns {
    /// Starts account sign-in with identity-only scopes.
    pub async fn start(&self, http: &Client, p: Provider) -> Result<Prompt, String> {
        let (url, scope) = match p {
            Provider::Github => ("https://github.com/login/device/code", "read:user"),
            Provider::Gitlab => ("https://gitlab.com/oauth/authorize_device", "read_user"),
        };
        self.begin(http, p, url, scope, &p.client_id()?).await
    }

    async fn begin(
        &self,
        http: &Client,
        p: Provider,
        url: &str,
        scope: &str,
        client: &str,
    ) -> Result<Prompt, String> {
        let response = http
            .post(url)
            .header("Accept", "application/json")
            .form(&[("client_id", client), ("scope", scope)])
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?;
        #[derive(Deserialize)]
        struct Device {
            device_code: String,
            user_code: String,
            verification_uri: String,
            expires_in: u64,
            interval: Option<u64>,
        }
        let d: Device = response
            .json()
            .await
            .map_err(|_| "Provider returned an invalid device authorization.")?;
        let url =
            reqwest::Url::parse(&d.verification_uri).map_err(|_| "Invalid verification URI.")?;
        if url.scheme() != "https"
            || url.host_str() != Some(p.host())
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
        {
            return Err("Unexpected verification URI.".into());
        }
        let mut active = self.0.lock().map_err(|_| "Device sign-in unavailable.")?;
        let now = Instant::now();
        active.retain(|_, c| c.expires > now);
        if active.len() >= 128 {
            return Err("Too many sign-ins in progress.".into());
        }
        let challenge = Uuid::new_v4();
        let interval = d.interval.unwrap_or(5).clamp(5, 60);
        active.insert(
            challenge,
            Pending {
                provider: p,
                code: d.device_code,
                expires: now + Duration::from_secs(d.expires_in.min(1200)),
                next: now + Duration::from_secs(interval),
                interval: Duration::from_secs(interval),
            },
        );
        Ok(Prompt {
            challenge,
            code: d.user_code,
            url: d.verification_uri,
            interval,
        })
    }
    /// Returns the signed-in account once the user confirms at the provider.
    pub async fn poll(
        &self,
        http: &Client,
        p: Provider,
        id: Uuid,
    ) -> Result<Option<Authorized>, String> {
        let me = match p {
            Provider::Github => "https://api.github.com/user",
            Provider::Gitlab => "https://gitlab.com/api/v4/user",
        };
        self.redeem(http, p, id, &p.client_id()?, p.token_url(), me)
            .await
    }

    async fn redeem(
        &self,
        http: &Client,
        p: Provider,
        id: Uuid,
        client: &str,
        token_url: &str,
        me: &str,
    ) -> Result<Option<Authorized>, String> {
        let code = {
            let mut active = self.0.lock().map_err(|_| "Device sign-in unavailable.")?;
            let c = active.get_mut(&id).ok_or("Sign-in expired. Start again.")?;
            let t = Instant::now();
            if c.provider != p || c.expires <= t {
                return Err("Sign-in expired. Start again.".into());
            }
            if t < c.next {
                return Ok(None);
            }
            c.next = t + c.interval;
            c.code.clone()
        };
        let response = http
            .post(token_url)
            .header("Accept", "application/json")
            .form(&[
                ("client_id", client),
                ("device_code", code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await
            .map_err(|e| e.to_string())?;
        #[derive(Deserialize)]
        struct Tokens {
            access_token: Option<String>,
            refresh_token: Option<String>,
            expires_in: Option<u64>,
            scope: Option<String>,
            error: Option<String>,
        }
        let tokens: Tokens = response
            .json()
            .await
            .map_err(|_| "Invalid provider token response.")?;
        if matches!(
            tokens.error.as_deref(),
            Some("authorization_pending" | "slow_down")
        ) {
            if tokens.error.as_deref() == Some("slow_down") {
                if let Some(c) = self
                    .0
                    .lock()
                    .map_err(|_| "Sign-in unavailable.")?
                    .get_mut(&id)
                {
                    c.interval += Duration::from_secs(5);
                    c.next = Instant::now() + c.interval;
                }
            }
            return Ok(None);
        }
        self.0
            .lock()
            .map_err(|_| "Sign-in unavailable.")?
            .remove(&id);
        let access = tokens
            .access_token
            .ok_or("Authorization expired or was declined.")?;
        #[derive(Deserialize)]
        struct User {
            id: i64,
            #[serde(alias = "username")]
            login: String,
        }
        let user: User = http
            .get(me)
            .bearer_auth(&access)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|_| "Provider account lookup failed.")?;
        Ok(Some(Authorized {
            login: user.login,
            account: user.id,
            access_token: access,
            refresh_token: tokens.refresh_token,
            expires_in: tokens.expires_in,
            scope: tokens.scope,
            client_id: client.to_owned(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        routing::{get, post},
        Router,
    };
    #[tokio::test]
    async fn github_and_gitlab_sign_in_once_and_return_separate_reusable_credentials() {
        for p in [Provider::Github, Provider::Gitlab] {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            let addr = listener.local_addr().unwrap();
            let fixture=Router::new()
                .route("/device",post(move||async move{serde_json::json!({"device_code":"secret-challenge","user_code":"ABCD","verification_uri":format!("https://{}/device",p.host()),"expires_in":600,"interval":5}).to_string()}))
                .route("/token",post(||async{r#"{"access_token":"provider-credential","refresh_token":"renewal","scope":"read_user"}"#}))
                .route("/user",get(move||async move{match p{Provider::Github=>r#"{"id":91,"login":"alice"}"#,Provider::Gitlab=>r#"{"id":91,"username":"alice"}"#}}));
            let server = tokio::spawn(async move { axum::serve(listener, fixture).await.unwrap() });
            let client = Client::builder().no_proxy().build().unwrap();
            let signins = SignIns::default();
            let prompt = signins
                .begin(
                    &client,
                    p,
                    &format!("http://{addr}/device"),
                    "read_user",
                    "app",
                )
                .await
                .unwrap();
            assert!(!serde_json::to_string(&prompt)
                .unwrap()
                .contains("secret-challenge"));
            signins
                .0
                .lock()
                .unwrap()
                .get_mut(&prompt.challenge)
                .unwrap()
                .next = Instant::now();
            let token_url = format!("http://{addr}/token");
            let user_url = format!("http://{addr}/user");
            let received = signins
                .redeem(&client, p, prompt.challenge, "app", &token_url, &user_url)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(received.login, "alice");
            assert_eq!(received.account, 91);
            assert_eq!(received.access_token, "provider-credential");
            assert_eq!(received.refresh_token.as_deref(), Some("renewal"));
            assert!(signins
                .redeem(&client, p, prompt.challenge, "app", &token_url, &user_url)
                .await
                .is_err());
            server.abort();
        }
    }
}
