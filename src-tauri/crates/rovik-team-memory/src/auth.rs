//! Issues an account session and authorizes each project request against
//! repository membership and its device binding. Only credential hashes are persisted.
use crate::Hub;
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

type Error = StatusCode;
const UNAVAILABLE: Error = StatusCode::SERVICE_UNAVAILABLE;

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub(super) enum Role {
    Reader,
    Editor,
    Admin,
}

use rovik_forge::Provider;

#[derive(Deserialize)]
struct Project {
    project: Uuid,
    provider: Provider,
    repository_id: u64,
    #[serde(default)]
    allow_personal_hosts: bool,
}
#[derive(Deserialize)]
pub(super) struct Grant {
    pub principal: String,
    pub replica: Uuid,
    pub role: Role,
}
#[derive(Deserialize)]
struct DevelopmentGrant {
    project: Uuid,
    token_sha256: String,
    #[serde(flatten)]
    grant: Grant,
}

fn now() -> Result<u64, Error> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs())
        .map_err(|_| UNAVAILABLE)
}
fn digest(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .filter(|t| t.len() >= 32)
}
fn json(value: impl Serialize) -> Result<Response, Error> {
    Ok((
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        serde_json::to_vec(&value).map_err(|_| UNAVAILABLE)?,
    )
        .into_response())
}

async fn config<T: serde::de::DeserializeOwned + Send + 'static>(
    path: PathBuf,
) -> Result<T, Error> {
    tokio::task::spawn_blocking(move || {
        serde_json::from_slice(&std::fs::read(path).map_err(|_| UNAVAILABLE)?)
            .map_err(|_| UNAVAILABLE)
    })
    .await
    .map_err(|_| UNAVAILABLE)?
}
async fn project(hub: &Hub, id: Uuid) -> Result<Project, Error> {
    config::<Vec<Project>>(hub.projects.clone())
        .await?
        .into_iter()
        .find(|p| p.project == id)
        .ok_or(StatusCode::FORBIDDEN)
}
fn accounts(path: &std::path::Path) -> Result<Connection, Error> {
    let c = Connection::open(path.join("devices.db")).map_err(|_| UNAVAILABLE)?;
    c.busy_timeout(Duration::from_secs(5))
        .map_err(|_| UNAVAILABLE)?;
    c.execute_batch("CREATE TABLE IF NOT EXISTS sessions (token TEXT PRIMARY KEY,provider TEXT NOT NULL,account INTEGER NOT NULL,login TEXT NOT NULL,expires INTEGER NOT NULL);
                      CREATE TABLE IF NOT EXISTS replicas (project TEXT NOT NULL,replica TEXT NOT NULL,principal TEXT NOT NULL,device TEXT NOT NULL,PRIMARY KEY(project,replica));").map_err(|_|UNAVAILABLE)?;
    Ok(c)
}

pub(super) async fn start(
    State(hub): State<Arc<Hub>>,
    Path(provider): Path<Provider>,
) -> Result<Response, Error> {
    let _permit = hub
        .workers
        .try_acquire()
        .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    json(
        hub.signins
            .start(&hub.http, provider)
            .await
            .map_err(|_| UNAVAILABLE)?,
    )
}

pub(super) async fn finish(
    State(hub): State<Arc<Hub>>,
    Path(provider): Path<Provider>,
    body: Bytes,
) -> Result<Response, Error> {
    #[derive(Deserialize)]
    struct Poll {
        challenge: Uuid,
    }
    let poll: Poll = serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    let Some(user) = hub
        .signins
        .poll(&hub.http, provider, poll.challenge)
        .await
        .map_err(|_| StatusCode::FORBIDDEN)?
    else {
        return Ok(StatusCode::ACCEPTED.into_response());
    };
    let raw = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let hash = digest(&raw);
    let directory = hub.directory.clone();
    let login = user.login.clone();
    let expiry = (now()? + 7 * 86400) as i64;
    tokio::task::spawn_blocking(move || {
        let c = accounts(&directory)?;
        c.execute(
            "INSERT INTO sessions VALUES (?1,?2,?3,?4,?5)",
            params![hash, provider.name(), user.account, login, expiry],
        )
        .map_err(|_| UNAVAILABLE)?;
        Ok::<_, Error>(())
    })
    .await
    .map_err(|_| UNAVAILABLE)??;

    let mut result = serde_json::to_value(user).map_err(|_| UNAVAILABLE)?;
    result["session"] = raw.into();
    json(result)
}

async fn membership(hub: &Hub, p: &Project, id: i64, login: &str) -> Result<Role, Error> {
    let token = std::env::var(match p.provider {
        Provider::Github => "ROVIK_TEAM_GITHUB_TOKEN",
        Provider::Gitlab => "ROVIK_TEAM_GITLAB_TOKEN",
    })
    .map_err(|_| UNAVAILABLE)?;
    let base = match p.provider {
        Provider::Github => "https://api.github.com",
        Provider::Gitlab => "https://gitlab.com/api/v4",
    };
    lookup_member(&hub.http, p, id, login, &token, base).await
}

async fn lookup_member(
    http: &reqwest::Client,
    p: &Project,
    id: i64,
    login: &str,
    token: &str,
    base: &str,
) -> Result<Role, Error> {
    async fn get(
        http: &reqwest::Client,
        url: &str,
        token: &str,
    ) -> Result<reqwest::Response, Error> {
        let response = http
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| UNAVAILABLE)?;
        match response.status().as_u16() {
            200..=299 => Ok(response),
            403 | 404 => Err(StatusCode::FORBIDDEN),
            _ => Err(UNAVAILABLE),
        }
    }
    match p.provider {
        Provider::Github => {
            #[derive(Deserialize)]
            struct Repo {
                id: u64,
                full_name: String,
            }
            let repo: Repo = get(
                http,
                &format!("{base}/repositories/{}", p.repository_id),
                token,
            )
            .await?
            .json()
            .await
            .map_err(|_| UNAVAILABLE)?;
            if repo.id != p.repository_id
                || !repo.full_name.split('/').all(valid_segment)
                || !valid_segment(login)
            {
                return Err(StatusCode::FORBIDDEN);
            }
            let url = format!("{base}/repos/{}/collaborators/{login}", repo.full_name);
            get(http, &url, token).await?;
            #[derive(Deserialize)]
            struct User {
                id: i64,
            }
            #[derive(Deserialize)]
            struct Access {
                user: User,
                permission: String,
            }
            let a: Access = get(http, &format!("{url}/permission"), token)
                .await?
                .json()
                .await
                .map_err(|_| UNAVAILABLE)?;
            if a.user.id != id {
                return Err(StatusCode::FORBIDDEN);
            }
            match a.permission.as_str() {
                "admin" => Ok(Role::Admin),
                "maintain" | "write" => Ok(Role::Editor),
                "triage" | "read" => Ok(Role::Reader),
                _ => Err(StatusCode::FORBIDDEN),
            }
        }
        Provider::Gitlab => {
            #[derive(Deserialize)]
            struct Member {
                id: i64,
                access_level: u32,
            }
            let a: Member = get(
                http,
                &format!("{base}/projects/{}/members/all/{id}", p.repository_id),
                token,
            )
            .await?
            .json()
            .await
            .map_err(|_| UNAVAILABLE)?;
            if a.id != id {
                return Err(StatusCode::FORBIDDEN);
            }
            match a.access_level {
                50.. => Ok(Role::Admin),
                30..=49 => Ok(Role::Editor),
                20..=29 => Ok(Role::Reader),
                _ => Err(StatusCode::FORBIDDEN),
            }
        }
    }
}
fn valid_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}

pub(super) async fn authorize(hub: &Hub, id: Uuid, headers: &HeaderMap) -> Result<Grant, Error> {
    let hash = digest(bearer(headers).ok_or(StatusCode::UNAUTHORIZED)?);
    if hub.development {
        let grants: Vec<DevelopmentGrant> = config(hub.grants.clone()).await?;
        if let Some(g) = grants
            .into_iter()
            .find(|g| g.project == id && g.token_sha256 == hash)
        {
            return Ok(g.grant);
        }
        return Err(StatusCode::FORBIDDEN);
    }
    let p = project(hub, id).await?;
    let directory = hub.directory.clone();
    let (provider, user, login): (String, i64, String) = tokio::task::spawn_blocking(move || {
        let c = accounts(&directory)?;
        c.query_row(
            "SELECT provider,account,login FROM sessions WHERE token=?1 AND expires>?2",
            params![hash, now()? as i64],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|_| UNAVAILABLE)?
        .ok_or(StatusCode::UNAUTHORIZED)
    })
    .await
    .map_err(|_| UNAVAILABLE)??;
    if provider != p.provider.name() || !p.allow_personal_hosts {
        return Err(StatusCode::FORBIDDEN);
    }
    let cache_key = (p.provider.name(), p.repository_id, user);
    let cached = hub
        .memberships
        .lock()
        .await
        .get(&cache_key)
        .copied()
        .filter(|(until, _)| *until > std::time::Instant::now());
    let role = if let Some((_, role)) = cached {
        role
    } else {
        let role = membership(hub, &p, user, &login).await?;
        let mut cache = hub.memberships.lock().await;
        cache.retain(|_, (until, _)| *until > std::time::Instant::now());
        cache.insert(
            cache_key,
            (std::time::Instant::now() + Duration::from_secs(300), role),
        );
        role
    };
    let replica = headers
        .get("x-rovik-replica")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::parse_str(v).ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let device = bearer_device(headers)?;
    let principal = format!("{provider}/{user}");
    let directory = hub.directory.clone();
    let owner = principal.clone();
    tokio::task::spawn_blocking(move || {
        let mut c = accounts(&directory)?;
        let tx = c
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| UNAVAILABLE)?;
        tx.execute(
            "INSERT OR IGNORE INTO replicas VALUES (?1,?2,?3,?4)",
            params![id.to_string(), replica.to_string(), owner, device],
        )
        .map_err(|_| UNAVAILABLE)?;
        let saved: (String, String) = tx
            .query_row(
                "SELECT principal,device FROM replicas WHERE project=?1 AND replica=?2",
                params![id.to_string(), replica.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| UNAVAILABLE)?;
        if saved != (owner, device) {
            return Err(StatusCode::CONFLICT);
        }
        tx.commit().map_err(|_| UNAVAILABLE)
    })
    .await
    .map_err(|_| UNAVAILABLE)??;
    Ok(Grant {
        role,
        replica,
        principal,
    })
}

fn bearer_device(headers: &HeaderMap) -> Result<String, Error> {
    let secret = headers
        .get("x-rovik-device")
        .and_then(|v| v.to_str().ok())
        .filter(|s| s.len() >= 32 && s.len() < 256)
        .ok_or(StatusCode::UNAUTHORIZED)?;
    Ok(digest(secret))
}

pub(super) async fn logout(
    State(hub): State<Arc<Hub>>,
    headers: HeaderMap,
) -> Result<StatusCode, Error> {
    let hash = digest(bearer(&headers).ok_or(StatusCode::UNAUTHORIZED)?);
    let directory = hub.directory.clone();
    tokio::task::spawn_blocking(move || {
        accounts(&directory)?
            .execute("DELETE FROM sessions WHERE token=?1", [hash])
            .map_err(|_| UNAVAILABLE)?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|_| UNAVAILABLE)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Router};

    #[tokio::test]
    async fn membership_requires_the_registered_repository_and_immutable_provider_user() {
        let routes = Router::new()
            .route(
                "/repositories/12",
                get(|| async { r#"{"id":12,"full_name":"team/project"}"# }),
            )
            .route(
                "/repos/team/project/collaborators/alice",
                get(|| async { StatusCode::NO_CONTENT }),
            )
            .route(
                "/repos/team/project/collaborators/alice/permission",
                get(|| async { r#"{"user":{"id":8},"permission":"write"}"# }),
            )
            .route(
                "/projects/12/members/all/8",
                get(|| async { r#"{"id":8,"access_level":20}"# }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
        let http = reqwest::Client::new();
        let mut project = Project {
            project: Uuid::new_v4(),
            provider: Provider::Github,
            repository_id: 12,
            allow_personal_hosts: true,
        };
        assert!(matches!(
            lookup_member(&http, &project, 8, "alice", "test", &base).await,
            Ok(Role::Editor)
        ));
        assert!(matches!(
            lookup_member(&http, &project, 9, "alice", "test", &base).await,
            Err(StatusCode::FORBIDDEN)
        ));
        project.provider = Provider::Gitlab;
        assert!(matches!(
            lookup_member(&http, &project, 8, "alice", "test", &base).await,
            Ok(Role::Reader)
        ));
        assert!(matches!(
            lookup_member(&http, &project, 9, "alice", "test", &base).await,
            Err(StatusCode::FORBIDDEN)
        ));
        server.abort();
    }
}
