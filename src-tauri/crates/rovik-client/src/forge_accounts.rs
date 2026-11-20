//! App-wide repository accounts. Provider credentials are backend-only. Project
//! services receive a separate session restricted to its original HTTPS issuer.
use crate::secrets::{SecretError, SecretName, SecretStore};
use rovik_protocol::Secret;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub use rovik_forge::Provider;
fn key(p: Provider) -> SecretName {
    SecretName::ForgeAccount {
        provider: p.name().into(),
    }
}
static DIRECT: std::sync::OnceLock<rovik_forge::SignIns> = std::sync::OnceLock::new();

/// One global settings action. OAuth access and refresh tokens never appear in replies.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AccountAction {
    Status,
    SignIn { issuer: String },
    Poll { issuer: String, challenge: Uuid },
    SignOut,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Account {
    issuer: String,
    login: String,
    account: i64,
    session: Secret,
    access_token: Secret,
    refresh_token: Option<Secret>,
    scope: Option<String>,
    expires_at: Option<u64>,
    client_id: String,
}

fn load(store: &SecretStore, provider: Provider) -> Result<Option<Account>, String> {
    match store.get(&key(provider)) {
        Ok(raw) => serde_json::from_str(&raw.0)
            .map(Some)
            .map_err(|_| "Saved repository account could not be decoded. Sign in again.".into()),
        Err(SecretError::Missing(_)) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}
fn save(store: &SecretStore, p: Provider, a: &Account) -> Result<(), String> {
    store
        .set(
            &key(p),
            &Secret(serde_json::to_string(a).map_err(|e| e.to_string())?),
        )
        .map_err(|e| e.to_string())
}
fn now() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs())
        .map_err(|e| e.to_string())
}
fn base(value: &str) -> Result<reqwest::Url, String> {
    let mut u = reqwest::Url::parse(value).map_err(|_| "Invalid sign-in service address")?;
    if u.scheme() != "https"
        || u.host().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
        || u.query().is_some()
    {
        return Err("Enter an HTTPS service URL without credentials, query or fragment.".into());
    }
    u.path_segments_mut()
        .map_err(|_| "Invalid sign-in address")?
        .pop_if_empty()
        .push("");
    Ok(u)
}
fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("rovik-ide")
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())
}
fn endpoint(issuer: &str, p: Provider, poll: bool) -> Result<reqwest::Url, String> {
    base(issuer)?
        .join(&format!(
            "v1/accounts/{}/signin{}",
            p.name(),
            if poll { "/poll" } else { "" }
        ))
        .map_err(|e| e.to_string())
}
/// Scoped session for the explicitly trusted service. This credential cannot
/// be used at the GitHub or GitLab APIs.
pub fn service_session(store: &SecretStore, p: Provider, issuer: &str) -> Result<Secret, String> {
    let a = load(store, p)?
        .ok_or_else(|| format!("Sign in to {} in Settings → Repository accounts.", p.name()))?;
    if base(issuer)?.as_str() != a.issuer {
        return Err("This hub differs from the account’s trusted sign-in service.".into());
    }
    Ok(a.session)
}

/// Retrieves a provider API credential for a backend operation. The caller
/// specifies its required scope and sends the result only to this provider.
/// Run on a blocking worker because the secret store may prompt the user.
pub fn provider_token(
    store: &SecretStore,
    p: Provider,
    required_scope: &str,
    runtime: &tokio::runtime::Handle,
) -> Result<Secret, String> {
    let _guard = store.forge_lock(p.name())?;
    let mut a = load(store, p)?.ok_or("Sign in to the repository provider first.")?;
    let refresh_before = now()?.saturating_add(60);
    if a.expires_at.is_some_and(|at| at <= refresh_before) {
        let refresh = a
            .refresh_token
            .as_ref()
            .ok_or("Repository sign-in expired. Sign in again.")?;
        let url = p.token_url();
        #[derive(Deserialize)]
        struct Renewed {
            access_token: Secret,
            refresh_token: Option<Secret>,
            expires_in: Option<u64>,
            scope: Option<String>,
        }
        let response = runtime
            .block_on(
                client()?
                    .post(url)
                    .header("Accept", "application/json")
                    .form(&[
                        ("client_id", a.client_id.as_str()),
                        ("grant_type", "refresh_token"),
                        ("refresh_token", refresh.0.as_str()),
                    ])
                    .send(),
            )
            .map_err(|e| e.to_string())?;
        let value: Renewed = runtime
            .block_on(
                response
                    .error_for_status()
                    .map_err(|e| e.to_string())?
                    .json(),
            )
            .map_err(|_| "Repository sign-in expired. Sign in again.")?;
        a.access_token = value.access_token;
        if value.refresh_token.is_some() {
            a.refresh_token = value.refresh_token;
        }
        if value.scope.is_some() {
            a.scope = value.scope;
        }
        let t = now()?;
        a.expires_at = value.expires_in.map(|s| t.saturating_add(s));
        save(store, p, &a)?;
    }
    if !required_scope.is_empty()
        && !a
            .scope
            .as_deref()
            .unwrap_or("")
            .split([',', ' '])
            .any(|s| s == required_scope)
    {
        return Err(format!(
            "This action requires an additional provider permission ({required_scope})."
        ));
    }
    Ok(a.access_token)
}

pub(crate) fn run(
    store: &SecretStore,
    p: Provider,
    action: AccountAction,
    runtime: tokio::runtime::Handle,
) -> Result<Value, String> {
    let _guard = store.forge_lock(p.name())?;
    match action {
        AccountAction::Status => Ok(load(store, p)?.map_or(
            Value::Null,
            |a| json!({"login":a.login,"issuer":a.issuer,"account":a.account,"scope":a.scope}),
        )),
        AccountAction::SignOut => {
            if let Some(a) = load(store, p)? {
                if a.issuer.is_empty() {
                    store.delete(&key(p)).map_err(|e| e.to_string())?;
                    return Ok(Value::Null);
                }
                let result = runtime.block_on(
                    client()?
                        .post(
                            base(&a.issuer)?
                                .join("v1/accounts/logout")
                                .map_err(|e| e.to_string())?,
                        )
                        .bearer_auth(&a.session.0)
                        .send(),
                );
                store.delete(&key(p)).map_err(|e| e.to_string())?;
                if !result.is_ok_and(|r| r.status().is_success()) {
                    return Err("Signed out on this desktop. The sign-in service could not confirm session revocation.".into());
                }
            }
            Ok(Value::Null)
        }
        AccountAction::SignIn { issuer } => {
            if issuer.is_empty() {
                return Ok(json!(runtime.block_on(
                    DIRECT.get_or_init(Default::default).start(&client()?, p)
                )?));
            }
            #[derive(Deserialize, Serialize)]
            struct Device {
                challenge: Uuid,
                code: String,
                url: String,
                interval: u64,
            }
            let res = runtime
                .block_on(client()?.post(endpoint(&issuer, p, false)?).send())
                .map_err(|e| e.to_string())?;
            let device: Device = runtime
                .block_on(res.error_for_status().map_err(|e| e.to_string())?.json())
                .map_err(|e| e.to_string())?;
            let url = base(&device.url)?;
            if url.host_str() != Some(p.host()) || url.port().is_some() {
                return Err("The sign-in service returned an invalid provider address.".into());
            }
            Ok(json!(device))
        }
        AccountAction::Poll { issuer, challenge } => {
            if issuer.is_empty() {
                let Some(v) = runtime.block_on(DIRECT.get_or_init(Default::default).poll(
                    &client()?,
                    p,
                    challenge,
                ))?
                else {
                    return Ok(json!({"pending":true}));
                };
                let t = now()?;
                save(
                    store,
                    p,
                    &Account {
                        issuer,
                        login: v.login,
                        account: v.account,
                        session: Secret(String::new()),
                        access_token: Secret(v.access_token),
                        refresh_token: v.refresh_token.map(Secret),
                        scope: v.scope,
                        expires_at: v.expires_in.map(|s| t.saturating_add(s)),
                        client_id: v.client_id,
                    },
                )?;
                return Ok(json!({"pending":false}));
            }
            let res = runtime
                .block_on(
                    client()?
                        .post(endpoint(&issuer, p, true)?)
                        .json(&json!({"challenge":challenge}))
                        .send(),
                )
                .map_err(|e| e.to_string())?;
            if res.status() == reqwest::StatusCode::ACCEPTED {
                return Ok(json!({"pending":true}));
            }
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Issued {
                login: String,
                account: i64,
                session: Secret,
                access_token: Secret,
                refresh_token: Option<Secret>,
                scope: Option<String>,
                expires_in: Option<u64>,
                client_id: String,
            }
            let v: Issued = runtime
                .block_on(res.error_for_status().map_err(|e| e.to_string())?.json())
                .map_err(|e| e.to_string())?;
            let t = now()?;
            save(
                store,
                p,
                &Account {
                    issuer: base(&issuer)?.to_string(),
                    login: v.login,
                    account: v.account,
                    session: v.session,
                    access_token: v.access_token,
                    refresh_token: v.refresh_token,
                    scope: v.scope,
                    expires_at: v.expires_in.map(|s| t.saturating_add(s)),
                    client_id: v.client_id,
                },
            )?;
            Ok(json!({"pending":false}))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn global_credentials_are_provider_scoped_and_only_hub_sessions_go_to_the_matching_issuer(
    ) {
        let directory = tempfile::tempdir().unwrap();
        let store = SecretStore::file(
            directory.path().join("accounts.json"),
            crate::secrets::Kdf {
                memory_kib: 32,
                iterations: 1,
                parallelism: 1,
            },
        );
        store.unlock(&Secret("a test passphrase".into())).unwrap();
        for provider in [Provider::Github, Provider::Gitlab] {
            let name = provider.name();
            save(
                &store,
                provider,
                &Account {
                    issuer: "https://hub.example/".into(),
                    login: format!("{name}-user"),
                    account: 1,
                    session: Secret(format!("{name}-session")),
                    access_token: Secret(format!("{name}-access")),
                    refresh_token: None,
                    scope: Some("read_user".into()),
                    expires_at: None,
                    client_id: "test".into(),
                },
            )
            .unwrap();
            assert_eq!(
                service_session(&store, provider, "https://hub.example")
                    .unwrap()
                    .0,
                format!("{name}-session")
            );
            assert!(service_session(&store, provider, "https://other.example").is_err());
            assert_eq!(
                provider_token(
                    &store,
                    provider,
                    "read_user",
                    &tokio::runtime::Handle::current()
                )
                .unwrap()
                .0,
                format!("{name}-access")
            );
            assert!(provider_token(
                &store,
                provider,
                "write_repository",
                &tokio::runtime::Handle::current()
            )
            .is_err());
            let status = run(
                &store,
                provider,
                AccountAction::Status,
                tokio::runtime::Handle::current(),
            )
            .unwrap();
            assert_eq!(status["login"], format!("{name}-user"));
            assert!(!status.to_string().contains("access"));
        }
        store.delete(&key(Provider::Github)).unwrap();
        assert!(load(&store, Provider::Github).unwrap().is_none());
        assert!(load(&store, Provider::Gitlab).unwrap().is_some());
    }
}
