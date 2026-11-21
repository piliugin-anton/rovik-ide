//! Per-folder links live in application data. The token is bound to a link
//! with an immutable destination, so project settings cannot redirect it.

use crate::forge_accounts::{self, Provider};
use crate::{desktop_backend::DesktopBackend, secrets::SecretName};
use rovik_core::{
    memory::store_path,
    restricted_files::{Op, RestrictedFiles},
};
use rovik_protocol::Secret;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{fs, path::Path};
use uuid::Uuid;
use verified_code_memory::{team::project_record, MemoryRecord, MemoryStore};

/// A settings or manager action on this desktop.
#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Action {
    /// Read the linked destination and replica ID.
    Status,
    /// Bind a fresh, empty replica to the given project.
    Link {
        hub: String,
        project: Uuid,
        provider: Provider,
    },
    /// Preview exactly one publication without enqueueing it.
    Preview { id: Uuid },
    /// Approve the exact revision shown in the preview.
    Publish { preview: Box<MemoryRecord> },
    /// Withdraw one mapped record from the team.
    Withdraw { id: Uuid },
    /// Keep the saved personal edit and acknowledge a conflict.
    Resolve { id: Uuid },
    /// Recheck previously blocked records against current project policy.
    RetryBlocked,
    /// Exchange bounded pages and materialize them locally.
    Sync,
    /// Bootstrap a replacement replica and preserve divergent records for review.
    Reconcile,
    /// Catch up an open project after reconnect or the polling interval.
    Tick,
    /// Suspend or resume exchanges.
    Pause { paused: bool },
}

#[derive(Serialize, Deserialize)]
struct Link {
    id: Uuid,
    hub: String,
    provider: Provider,
    project: Uuid,
    paused: bool,
    last_sync: Option<u64>,
    #[serde(default)]
    last_error: Option<String>,
    #[serde(default)]
    replica_file: Option<Uuid>,
    #[serde(default)]
    reconciling: bool,
    #[serde(default)]
    retry_after: u64,
}

pub(super) async fn run(
    backend: &DesktopBackend,
    root: String,
    action: Action,
) -> Result<Value, String> {
    let paths = backend.core.paths.clone();
    let secrets = backend.secrets.clone();
    let runtime = tokio::runtime::Handle::current();
    super::super::blocking(move || run_on_worker(paths, secrets, runtime, root, action)).await
}

fn run_on_worker(
    paths: rovik_core::Paths,
    secrets: std::sync::Arc<crate::secrets::SecretStore>,
    runtime: tokio::runtime::Handle,
    root: String,
    action: Action,
) -> Result<Value, String> {
    let app_path = store_path(&paths, Path::new(&root));
    let dir = app_path.with_extension("team");
    let config = dir.join("link.json");
    if matches!(action, Action::Status | Action::Tick) && !config.exists() {
        return Ok(Value::Null);
    }
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // Status reads an atomically replaced configuration and may run during network I/O.
    let _guard = if matches!(action, Action::Status) {
        None
    } else {
        Some(
            rovik_core::os_lock::try_hold(&dir.join("exchange.lock"))
                .map_err(|e| e.to_string())?
                .ok_or("This project's team memory is busy. Try again shortly.")?,
        )
    };
    if let Action::Link {
        hub,
        project,
        provider,
    } = action
    {
        if config.exists() {
            return Err("This folder already has a team link.".into());
        }
        let url = reqwest::Url::parse(&hub).map_err(|e| e.to_string())?;
        if url.scheme() != "https"
            || url.host().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.query().is_some()
        {
            return Err("Enter an HTTPS hub URL without credentials, query or fragment.".into());
        }
        let id = Uuid::new_v4();
        let device = Secret(format!(
            "{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        ));
        // Verify the account and project before making the folder binding durable.
        let session = forge_accounts::service_session(&secrets, provider, &hub)?;
        let file = dir.join(format!("{id}.db"));
        let mut replica = MemoryStore::open(&file).map_err(|e| e.to_string())?;
        let client = rovik_team_memory::HubClient::new(&hub, project, &session.0, &device.0)?;
        let discovery = runtime.block_on(rovik_team_memory::sync_once(
            &mut replica,
            id,
            &client,
            false,
            |_| Ok(false),
        ));
        drop(replica);
        if let Err(error) = discovery {
            fs::remove_file(&file)
                .map_err(|e| format!("{error}. Could not remove unused replica: {e}"))?;
            return Err(error);
        }
        secrets
            .set(
                &SecretName::TeamMemory {
                    link: id.to_string(),
                },
                &device,
            )
            .map_err(|e| e.to_string())?;
        save(
            &config,
            &Link {
                id,
                provider,
                hub: url.to_string(),
                project,
                paused: false,
                last_sync: None,
                last_error: None,
                replica_file: Some(id),
                reconciling: false,
                retry_after: 0,
            },
        )?;
        return Ok(Value::Null);
    }
    let mut link: Link = serde_json::from_slice(&fs::read(&config).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    if matches!(action, Action::Tick) && (link.paused || now < link.retry_after) {
        return Ok(Value::Null);
    }
    let key = SecretName::TeamMemory {
        link: link.id.to_string(),
    };
    if matches!(action, Action::Reconcile) && !link.reconciling {
        link.replica_file = Some(Uuid::new_v4());
        secrets
            .set(
                &key,
                &Secret(format!(
                    "{}{}",
                    Uuid::new_v4().simple(),
                    Uuid::new_v4().simple()
                )),
            )
            .map_err(|e| e.to_string())?;
        link.reconciling = true;
        save(&config, &link)?;
    }
    let name = link
        .replica_file
        .map_or_else(|| "replica.db".to_string(), |id| format!("{id}.db"));
    let mut replica = MemoryStore::open(dir.join(name)).map_err(|e| e.to_string())?;
    let app = MemoryStore::open(app_path).map_err(|e| e.to_string())?;
    let settings = rovik_core::project_profile::load_project_settings(&paths.config_dir, &root);
    let restricted = RestrictedFiles::from_settings(&settings);
    let readable = |file: &str| restricted.blocked(&root, file, Op::Read).is_none();
    match action {
        Action::Status => {
            let mut blocked = app
                .team_blocked(&root, link.id)
                .map_err(|e| e.to_string())?;
            blocked.extend(
                replica
                    .team_blocked(&link.id.to_string(), link.id)
                    .map_err(|e| e.to_string())?,
            );
            Ok(
                json!({"hub":link.hub,"provider":link.provider,"project":link.project,"replica":replica.sync_replica().map_err(|e|e.to_string())?,"paused":link.paused,"lastSync":link.last_sync,"lastError":link.last_error,"blocked":blocked,"conflicts":app.team_conflicts(&root,link.id).map_err(|e|e.to_string())?}),
            )
        }
        Action::RetryBlocked => {
            app.team_retry_blocked(&root, link.id, &replica)
                .map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }
        Action::Pause { paused } => {
            link.paused = paused;
            save(&config, &link)?;
            Ok(Value::Null)
        }
        Action::Withdraw { id } => {
            app.team_withdraw(&root, link.id, id)
                .map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }
        Action::Resolve { id } => {
            app.team_resolve(&root, link.id, id)
                .map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }
        Action::Preview { id } => {
            let record = app.get(id).map_err(|e| e.to_string())?;
            if record.repo != root {
                return Err("Memory belongs to another project.".into());
            }
            Ok(json!(
                project_record(&record, readable).map_err(|e| e.to_string())?
            ))
        }
        Action::Publish { preview } => {
            app.team_queue_publication(&root, link.id, &preview, readable)
                .map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }
        Action::Tick | Action::Sync | Action::Reconcile => {
            if link.paused {
                return Err("Resume team sharing before syncing.".into());
            }
            let link_id = link.id;
            let check = |change: &verified_code_memory::sync::Change| match app
                .team_check_push(&root, link_id, change, readable)
            {
                Ok(()) => Ok(true),
                Err(verified_code_memory::VcmError::InvalidSync(reason)) => {
                    app.team_block(&root, link_id, change.id, reason)
                        .map_err(|e| e.to_string())?;
                    Ok(false)
                }
                Err(verified_code_memory::VcmError::NotFound(_)) => Ok(false),
                Err(e) => Err(e.to_string()),
            };
            let mut server_delay = 0;
            let result: Result<Value, String> = (|| {
                let device = secrets.get(&key).map_err(|e| e.to_string())?;
                let token = forge_accounts::service_session(&secrets, link.provider, &link.hub)?;
                let client = rovik_team_memory::HubClient::new(
                    &link.hub,
                    link.project,
                    &token.0,
                    &device.0,
                )?;
                let exchange_result = (|| {
                    runtime.block_on(rovik_team_memory::bootstrap::receive(
                        &replica, link.id, &client,
                    ))?;
                    if link.reconciling {
                        app.team_rebind(&root, link.id, &replica)
                            .map_err(|e| e.to_string())?;
                        link.reconciling = false;
                        save(&config, &link)?;
                    }
                    // Pull first: admin clears and offline edits are reconciled before any push.
                    runtime.block_on(rovik_team_memory::sync_once(
                        &mut replica,
                        link.id,
                        &client,
                        false,
                        check,
                    ))?;
                    materialize(&app, &replica, &root, link.id, &readable)?;
                    for _ in 0..64 {
                        if !app
                            .team_scan_approved(&root, link.id, readable)
                            .map_err(|e| e.to_string())?
                        {
                            break;
                        }
                    }
                    for _ in 0..64 {
                        if !app
                            .team_flush_publication(&root, link.id, &replica, readable)
                            .map_err(|e| e.to_string())?
                        {
                            break;
                        }
                    }
                    for _ in 0..64 {
                        let more = runtime.block_on(rovik_team_memory::sync_once(
                            &mut replica,
                            link.id,
                            &client,
                            true,
                            check,
                        ))?;
                        materialize(&app, &replica, &root, link.id, &readable)?;
                        if !more {
                            return Ok(Value::Null);
                        }
                    }
                    Err("Progress saved. Sync again to continue.".into())
                })();
                server_delay = client.retry_after();
                exchange_result
            })();
            link.last_error = result.as_ref().err().cloned();
            if result.is_ok() {
                link.last_sync = Some(now);
            }
            link.retry_after = if result.is_ok() {
                0
            } else {
                now + server_delay.max(60) + u64::from(link.id.as_bytes()[0] % 10)
            };
            save(&config, &link)?;
            result
        }
        Action::Link { .. } => Err("Team link already exists.".into()),
    }
}

fn materialize(
    app: &MemoryStore,
    replica: &MemoryStore,
    root: &str,
    link: Uuid,
    readable: &impl Fn(&str) -> bool,
) -> Result<(), String> {
    replica
        .team_restore_pending(link, app, root)
        .map_err(|e| e.to_string())?;
    let mut cursor = app
        .team_import_cursor(root, link)
        .map_err(|e| e.to_string())?;
    loop {
        let mut page = replica
            .sync_export(&link.to_string(), cursor)
            .map_err(|e| e.to_string())?;
        let mut visible = Vec::new();
        for change in std::mem::take(&mut page.changes) {
            if change
                .record
                .as_ref()
                .is_some_and(|r| !r.files.iter().all(|f| readable(f)))
            {
                app.team_block(
                    root,
                    link,
                    change.id,
                    "incoming revision references a restricted file",
                )
                .map_err(|e| e.to_string())?;
            } else {
                visible.push(change);
            }
        }
        page.changes = visible;
        let applicability = page
            .changes
            .iter()
            .filter_map(|c| c.record.as_ref())
            .filter(|r| r.files.iter().all(|f| readable(f)))
            .map(|r| {
                (
                    r.id,
                    rovik_core::agent_run::memory::team_applicability(r, root),
                )
            })
            .collect();
        let mut authors = std::collections::HashMap::new();
        for change in &page.changes {
            if let Some(names) = replica
                .team_authors(&link.to_string(), change.id)
                .map_err(|e| e.to_string())?
            {
                authors.insert(change.id, names);
            }
        }
        app.team_import_attributed(root, link, cursor, &page, &applicability, &authors)
            .map_err(|e| e.to_string())?;
        cursor = page.through;
        if !page.more {
            break;
        }
    }
    Ok(())
}

fn save(path: &Path, link: &Link) -> Result<(), String> {
    rovik_core::atomic_file::write_atomic_private(
        path,
        &serde_json::to_vec(link).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
