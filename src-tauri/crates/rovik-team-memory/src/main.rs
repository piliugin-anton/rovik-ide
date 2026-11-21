//! Self-hosted single-node team hub. A TLS reverse proxy fronts loopback.

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{header, HeaderMap, StatusCode},
    routing::post,
    Router,
};
use rovik_team_memory::{Exchange, Reply};
mod auth;
mod snapshot;
use auth::Role;

use std::{path::PathBuf, sync::Arc};
use uuid::Uuid;
use verified_code_memory::{sync::MAX_PAGE_BYTES, MemoryStore};

type MembershipCache =
    std::collections::HashMap<(&'static str, u64, i64), (std::time::Instant, Role)>;

struct Hub {
    directory: PathBuf,
    grants: PathBuf,
    projects: PathBuf,
    signins: rovik_forge::SignIns,
    memberships: tokio::sync::Mutex<MembershipCache>,
    http: reqwest::Client,
    development: bool,
    workers: tokio::sync::Semaphore,
    // A bounded set of locks serializes exchanges touching the same SQLite database.
    writers: [std::sync::Mutex<()>; 32],
    snapshots: Arc<tokio::sync::Semaphore>,
}

impl Hub {
    fn project_lock(&self, project: Uuid) -> std::sync::LockResult<std::sync::MutexGuard<'_, ()>> {
        let shard = usize::from(project.as_bytes()[0]) % self.writers.len();
        self.writers[shard].lock()
    }
}

async fn exchange(
    State(hub): State<Arc<Hub>>,
    Path(project): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<([(header::HeaderName, &'static str); 1], Vec<u8>), StatusCode> {
    let permit = hub.clone();
    let _permit = permit
        .workers
        .try_acquire()
        .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    let grant = auth::authorize(&hub, project, &headers).await?;
    tokio::task::spawn_blocking(move || {
        let request: Exchange =
            serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
        if request.clear.is_some() && !matches!(grant.role, Role::Admin) {
            return Err(StatusCode::FORBIDDEN);
        }
        if request.clear.is_some() && (request.push.is_some() || request.hub.is_none()) {
            return Err(StatusCode::BAD_REQUEST);
        }
        if request.push.is_some() && matches!(grant.role, Role::Reader) {
            return Err(StatusCode::FORBIDDEN);
        }
        if request.after < 0
            || request.hub.is_none() && (request.push.is_some() || request.after != 0)
        {
            return Err(StatusCode::BAD_REQUEST);
        }
        let _writer = hub.project_lock(project)
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let repo = project.to_string();
        let store = MemoryStore::open(hub.directory.join(format!("{project}.db")))
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let identity = store
            .sync_replica()
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        if request.hub.as_ref().is_some_and(|h| h != &identity) {
            return Err(StatusCode::CONFLICT);
        }
        if let Some(clear) = request.clear {
            store
                .team_admin_clear(
                    &repo,
                    &grant.principal,
                    &identity,
                    &clear.epoch,
                    clear.operation,
                )
                .map_err(|_| StatusCode::CONFLICT)?;
        }
        let mut rejected = Vec::new();
        let pushed = request.push.as_ref().map_or(0, |p| p.changes.len());
        if let Some(mut push) = request.push {
            if push.changes.len() > verified_code_memory::sync::MAX_PAGE_RECORDS {
                return Err(StatusCode::PAYLOAD_TOO_LARGE);
            }
            let readable = |path: &str| {
                // Deny credential files even for a misconfigured client.
                !path.split(['/', '\\']).any(|part| {
                    part.starts_with(".env")
                        || matches!(
                            part,
                            ".ssh"
                                | ".aws"
                                | ".gnupg"
                                | ".git"
                                | ".netrc"
                                | "id_rsa"
                                | "id_ed25519"
                        )
                        || part.ends_with(".pem")
                        || part.ends_with(".key")
                })
            };
            let changes = std::mem::take(&mut push.changes);
            let device = grant.replica.to_string();
            store
                .team_accept(
                    &repo,
                    &grant.principal,
                    &device,
                    &identity,
                    &push,
                    readable,
                )
                .map_err(|_| StatusCode::CONFLICT)?;
            for change in changes {
                let id = change.id;
                push.changes.push(change);
                match store.team_accept(
                    &repo,
                    &grant.principal,
                    &device,
                    &identity,
                    &push,
                    readable,
                ) {
                    Ok(()) => {}
                    Err(verified_code_memory::VcmError::InvalidSync(
                        "hub identity or epoch changed",
                    )) => return Err(StatusCode::CONFLICT),
                    Err(verified_code_memory::VcmError::InvalidSync(reason)) => {
                        rejected.push((id, reason.to_owned()))
                    }
                    Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
                }
                push.changes.clear();
            }
        }
        let page = store
            .sync_export(&repo, request.after)
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        let mut attribution = Vec::new();
        for change in &page.changes {
            if let Some((author, editor)) = store
                .team_provenance(&repo, change)
                .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
            {
                attribution.push((change.id, author, editor));
            }
        }
        eprintln!("{}",serde_json::json!({"event":"team_exchange","project":project,"pull_after":request.after,"pull_through":page.through,"more":page.more,"pushed":pushed,"rejected":rejected.len()}));
        Ok((
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::to_vec(&Reply {
                page,
                rejected,
                attribution,
            })
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        ))
    })
    .await
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
}

#[derive(serde::Deserialize)]
struct Purge {
    hub: String,
    epoch: verified_code_memory::sync::Stamp,
}

async fn purge(
    State(hub): State<Arc<Hub>>,
    Path((project, record)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, StatusCode> {
    let _permit = hub
        .workers
        .try_acquire()
        .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    let grant = auth::authorize(&hub, project, &headers).await?;
    if !matches!(grant.role, Role::Admin) {
        return Err(StatusCode::FORBIDDEN);
    }
    let request: Purge = serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    let state = hub.clone();
    tokio::task::spawn_blocking(move || {
        let _writer = state
            .project_lock(project)
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let store = MemoryStore::open(state.directory.join(format!("{project}.db")))
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        store
            .team_purge(
                &project.to_string(),
                &grant.principal,
                &request.hub,
                &request.epoch,
                record,
            )
            .map_err(|_| StatusCode::CONFLICT)?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = PathBuf::from(std::env::var("ROVIK_TEAM_DATA")?);
    if let Some(command) = std::env::args().nth(1) {
        if command != "rotate-restored-project" || std::env::args().count() != 3 {
            return Err("Usage: rovik-team-memory rotate-restored-project PROJECT_UUID".into());
        }
        let id = std::env::args()
            .nth(2)
            .ok_or("Missing project UUID")?
            .parse::<Uuid>()?;
        let path = directory.join(format!("{id}.db"));
        if !path.is_file() {
            return Err("Restored database does not exist".into());
        }
        let store = MemoryStore::open(path)?;
        println!(
            "New restored hub identity {}",
            store.team_rotate_restored_hub()?
        );
        return Ok(());
    }
    let grants = std::env::var_os("ROVIK_TEAM_GRANTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| directory.join("dev-grants.json"));
    let projects = std::env::var_os("ROVIK_TEAM_PROJECTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| directory.join("projects.json"));
    std::fs::create_dir_all(&directory)?;
    let app = Router::new()
        .route("/v1/projects/{project}/exchange", post(exchange))
        .route("/v1/projects/{project}/records/{record}/purge", post(purge))
        .route("/v1/projects/{project}/snapshot", post(snapshot::stream))
        .route("/v1/accounts/{provider}/signin", post(auth::start))
        .route("/v1/accounts/{provider}/signin/poll", post(auth::finish))
        .route("/v1/accounts/logout", post(auth::logout))
        .layer(DefaultBodyLimit::max(MAX_PAGE_BYTES + 2048))
        .with_state(Arc::new(Hub {
            directory,
            grants,
            projects,
            signins: Default::default(),
            memberships: Default::default(),
            http: reqwest::Client::builder()
                .https_only(true)
                .redirect(reqwest::redirect::Policy::none())
                .user_agent("rovik-team-memory")
                .timeout(std::time::Duration::from_secs(8))
                .build()?,
            development: std::env::var("ROVIK_TEAM_DEVELOPMENT").as_deref() == Ok("1"),
            writers: Default::default(),
            workers: tokio::sync::Semaphore::new(16),
            snapshots: Arc::new(tokio::sync::Semaphore::new(4)),
        }));
    let listener = tokio::net::TcpListener::bind((
        "127.0.0.1",
        std::env::var("ROVIK_TEAM_PORT")
            .unwrap_or_else(|_| "7878".into())
            .parse::<u16>()?,
    ))
    .await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use rovik_team_memory::{
        bootstrap::{Frame, Request},
        Clear, Reply,
    };
    use sha2::{Digest, Sha256};
    use verified_code_memory::{team::project_record, NewMemory, SourceKind};

    #[tokio::test]
    async fn authorized_clients_exchange_revisions_and_readers_bootstrap_without_write_access() {
        let dir = tempfile::tempdir().unwrap();
        let project = Uuid::new_v4();
        let scope = Uuid::new_v4();
        let app = MemoryStore::open_in_memory().unwrap();
        let replica = MemoryStore::open_in_memory().unwrap();
        let writer = replica.sync_replica().unwrap();
        let record = app
            .remember(
                NewMemory::project_fact("personal", "Build", "Run checks")
                    .with_source_kind(SourceKind::UserAssertion),
            )
            .unwrap();
        let preview = project_record(&record, |_| true).unwrap();
        app.team_queue_publication("personal", scope, &preview, |_| true)
            .unwrap();
        app.team_flush_publication("personal", scope, &replica, |_| true)
            .unwrap();
        let hash = |token: &str| {
            Sha256::digest(token.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        let editor = "e".repeat(40);
        let reader = "r".repeat(40);
        let admin = "a".repeat(40);
        let grants = dir.path().join("grants.json");
        let entries:Vec<_>=[(&editor,"editor"),(&reader,"reader"),(&admin,"admin")].into_iter().map(|(t,role)|serde_json::json!({"project":project,"principal":role,"replica":writer,"token_sha256":hash(t),"role":role})).collect();
        std::fs::write(&grants, serde_json::to_vec(&entries).unwrap()).unwrap();
        let hub = Arc::new(Hub {
            directory: dir.path().into(),
            grants: grants.clone(),
            projects: dir.path().join("projects"),
            development: true,
            writers: Default::default(),
            workers: tokio::sync::Semaphore::new(4),
            http: reqwest::Client::new(),
            signins: Default::default(),
            memberships: Default::default(),
            snapshots: Arc::new(tokio::sync::Semaphore::new(1)),
        });
        let headers = |t: &str| {
            let mut h = HeaderMap::new();
            h.insert(
                header::AUTHORIZATION,
                format!("Bearer {t}").parse().unwrap(),
            );
            h
        };
        let call = |token: &str, project: Uuid, request: Exchange| {
            exchange(
                State(hub.clone()),
                Path(project),
                headers(token),
                Bytes::from(serde_json::to_vec(&request).unwrap()),
            )
        };
        let discover = || Exchange {
            hub: None,
            after: 0,
            push: None,
            clear: None,
        };
        assert_eq!(
            call(&reader, Uuid::new_v4(), discover()).await.unwrap_err(),
            StatusCode::FORBIDDEN
        );
        let (_, initial) = call(&reader, project, discover()).await.unwrap();
        let initial: Reply = serde_json::from_slice(&initial).unwrap();
        let identity = initial.page.replica;
        let upload = || Exchange {
            hub: Some(identity.clone()),
            after: 0,
            push: Some(replica.sync_export(&scope.to_string(), 0).unwrap()),
            clear: None,
        };
        assert_eq!(
            call(&reader, project, upload()).await.unwrap_err(),
            StatusCode::FORBIDDEN
        );
        for _ in 0..2 {
            let (_, data) = call(&editor, project, upload()).await.unwrap();
            let response: Reply = serde_json::from_slice(&data).unwrap();
            assert_eq!(response.page.changes.len(), 1);
            assert_eq!(response.attribution[0].1, "editor");
            assert!(response.rejected.is_empty());
        }
        let snap = snapshot::stream(
            State(hub.clone()),
            Path(project),
            headers(&reader),
            Bytes::from(
                serde_json::to_vec(&Request {
                    replica: None,
                    after: 0,
                })
                .unwrap(),
            ),
        )
        .await
        .unwrap();
        // Keeping the first stream open holds its bounded worker until the reader
        // consumes frames. A second bootstrap can retry without delaying exchanges.
        let extra = snapshot::stream(
            State(hub.clone()),
            Path(project),
            headers(&reader),
            Bytes::from(
                serde_json::to_vec(&Request {
                    replica: None,
                    after: 0,
                })
                .unwrap(),
            ),
        )
        .await;
        assert!(matches!(extra, Err(StatusCode::TOO_MANY_REQUESTS)));
        assert!(call(&reader, project, discover()).await.is_ok());
        let bytes = to_bytes(snap.into_body(), 1024 * 1024).await.unwrap();
        let lines: Vec<_> = bytes.split_inclusive(|b| *b == b'\n').collect();
        assert_eq!(lines.len(), 3);
        let mut digest = Sha256::new();
        digest.update(lines[0]);
        digest.update(lines[1]);
        let Frame::Trailer {
            through,
            digest: actual,
        } = serde_json::from_slice(lines[2]).unwrap()
        else {
            panic!("missing trailer")
        };
        assert!(through > 0);
        assert_eq!(actual, rovik_team_memory::bootstrap::checksum(digest));
        let op = Uuid::new_v4();
        let clear = || Exchange {
            hub: Some(identity.clone()),
            after: 0,
            push: None,
            clear: Some(Clear {
                epoch: initial.page.epoch.clone(),
                operation: op,
            }),
        };
        assert_eq!(
            call(&editor, project, clear()).await.unwrap_err(),
            StatusCode::FORBIDDEN
        );
        for _ in 0..2 {
            let (_, data) = call(&admin, project, clear()).await.unwrap();
            let response: Reply = serde_json::from_slice(&data).unwrap();
            assert!(response.page.changes.is_empty());
            assert!(response.page.epoch > initial.page.epoch);
        }
        let record_id = upload().push.unwrap().changes[0].id;
        let after_clear: Reply =
            serde_json::from_slice(&call(&reader, project, discover()).await.unwrap().1).unwrap();
        let purge_request = |token: &str| {
            purge(
                State(hub.clone()),
                Path((project, record_id)),
                headers(token),
                Bytes::from(
                    serde_json::to_vec(
                        &serde_json::json!({"hub":identity,"epoch":after_clear.page.epoch}),
                    )
                    .unwrap(),
                ),
            )
        };
        assert_eq!(
            purge_request(&editor).await.unwrap_err(),
            StatusCode::FORBIDDEN
        );
        for _ in 0..2 {
            assert_eq!(purge_request(&admin).await.unwrap(), StatusCode::NO_CONTENT);
        }
        std::fs::write(grants, "[]").unwrap();
        // The restricted development fixture contains no production grants.
        assert!(call(&reader, project, discover()).await.is_err());
    }
}
