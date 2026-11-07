//! Project memory travels independently of code snapshots. Durable cursors are
//! acknowledged only after both sides have committed, so reconnect/retry is
//! idempotent and a replaced host database receives the complete history.
use super::*;
use rovik_core::chat_db::projects;
use rovik_core::memory::{
    sync::{Page, Progress},
    SharedMemoryStore,
};
use serde_json::Value;

impl Remote {
    async fn memory_page(
        &self,
        client: &Client,
        project: &str,
        progress: &Progress,
        changes: Option<Value>,
    ) -> Result<Page, String> {
        let reply = client
            .call_within(
                Request::MemorySync {
                    project: project.into(),
                    replica: (!progress.replica.is_empty()).then(|| progress.replica.clone()),
                    after: progress.received,
                    changes,
                },
                HOSTS_WAIT,
            )
            .await
            .map_err(|e| e.message)?;
        let value = Value::try_from(reply).map_err(|e| e.message)?;
        serde_json::from_value(value)
            .map_err(|e| format!("The host returned an invalid memory page: {e}"))
    }

    pub(super) async fn sync_memory(
        &self,
        host: &str,
        project: &str,
        root: &Path,
    ) -> Result<(), String> {
        let client = self.client(host).map_err(|e| e.message)?;
        if !client
            .daemon()
            .is_some_and(|d| d.serves(features::MEMORY_SYNC))
        {
            return Ok(());
        }
        let key = (host.to_string(), project.to_string());
        let lock = self.lock().memory_syncs.entry(key).or_default().clone();
        let work = async {
            let _guard = lock.lock().await;
            let core = self.core.clone();
            let folder = root.to_path_buf();
            let (store, repo, settings) = rovik_core::rt::spawn_blocking(move || {
                let store = core
                    .memory
                    .open(&core.paths, &folder)
                    .ok_or("The local project's memory couldn't be opened.")?;
                let repo = core.memory.repo(&folder);
                let settings = project_profile::load_project_settings(
                    &core.paths.config_dir,
                    &folder.to_string_lossy(),
                );
                Ok::<_, String>((store, repo, settings))
            })
            .await
            .map_err(|e| e.to_string())??;
            // Revisit previously suppressed records if file restrictions change.
            use std::hash::{Hash, Hasher};
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            settings.get("restrictedFiles").hash(&mut hash);
            let peer = format!("{host}:{:016x}", hash.finish());
            let s = store.clone();
            let r = repo.clone();
            let p = peer.clone();
            let mut progress = rovik_core::rt::spawn_blocking(move || s.sync_progress(&r, &p))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            let initial = self.memory_page(&client, project, &progress, None).await?;
            if initial.replica != progress.replica {
                progress = Progress {
                    replica: initial.replica.clone(),
                    ..Default::default()
                };
            }
            apply(&store, &repo, &peer, &mut progress, initial).await?;
            for _ in 0..256 {
                let s = store.clone();
                let r = repo.clone();
                let after = progress.sent;
                let folder = root.to_path_buf();
                let export_settings = settings.clone();
                let outgoing = rovik_core::rt::spawn_blocking(move || {
                    let mut page = s.sync_export(&r, after).map_err(|e| e.to_string())?;
                    rovik_core::memory::filter_sync_page(&mut page, &folder, &export_settings);
                    Ok::<_, String>(page)
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
                let reply = self
                    .memory_page(
                        &client,
                        project,
                        &progress,
                        Some(serde_json::to_value(&outgoing).map_err(|e| e.to_string())?),
                    )
                    .await?;
                if reply.replica != progress.replica {
                    return Err(
                        "The host's memory changed identity during synchronization; retry.".into(),
                    );
                }
                let more = outgoing.more || reply.more;
                progress.sent = outgoing.through;
                apply(&store, &repo, &peer, &mut progress, reply).await?;
                if !more {
                    return Ok(());
                }
            }
            Err(
                "Memory synchronization has more pages; progress is saved for the next retry."
                    .into(),
            )
        };
        tokio::time::timeout(Duration::from_secs(30), work)
            .await
            .map_err(|_| {
                "Memory synchronization timed out; progress is saved for the next retry."
                    .to_string()
            })?
    }

    pub(super) async fn sync_memory_chat(&self, host: &str, dialog: &str) -> Result<(), String> {
        let Some((project, root)) = self.project_of(dialog).await? else {
            return Ok(());
        };
        self.sync_memory(host, &project, &root).await
    }

    pub(super) async fn memory_sync_hint(&self, host: &str, dialog: &str) {
        if let Err(error) = self.sync_memory_chat(host, dialog).await {
            log::warn!("{host}: project memory synchronization failed: {error}");
            self.core.emit(AgentEvent::Notice {
                dialog_id: Some(dialog.into()),
                kind: "warning".into(),
                title: "Memory synchronization".into(),
                message: format!(
                    "Project memory isn't synchronized with {}: {error}. A later sync will retry.",
                    self.host_name(host)
                ),
            });
        }
    }

    pub(super) async fn sync_host_memories(&self, host: &str) {
        if !self.serves(host, features::MEMORY_SYNC) {
            return;
        }
        let chats: Vec<_> = self
            .lock()
            .chats
            .iter()
            .filter(|(_, h)| h.as_str() == host)
            .map(|(chat, _)| chat.clone())
            .collect();
        let mut seen = HashSet::new();
        for chat in chats {
            if let Ok(Some((project, root))) = self.project_of(&chat).await {
                if seen.insert(project.clone()) {
                    if let Err(error) = self.sync_memory(host, &project, &root).await {
                        log::warn!("{host}: project memory synchronization failed: {error}");
                    }
                }
            }
        }
        // Keep replicas known after their last chat moved back and after an
        // app restart. The acknowledgement table is durable; session caches
        // alone cannot discover these projects on reconnection.
        match self.core.db.read(projects::list).await {
            Ok(projects) => {
                for project in projects {
                    if project.missing || !seen.insert(project.id.clone()) {
                        continue;
                    }
                    let root = PathBuf::from(project.path);
                    match self.remembered_memory_hosts(&root).await {
                        Ok(hosts) if hosts.contains(host) => {
                            if let Err(error) = self.sync_memory(host, &project.id, &root).await {
                                log::warn!(
                                    "{host}: project memory synchronization failed: {error}"
                                );
                            }
                        }
                        Err(error) => log::warn!("{host}: memory peers couldn't be read: {error}"),
                        _ => {}
                    }
                }
            }
            Err(error) => log::warn!("{host}: memory projects couldn't be read: {error}"),
        }
    }

    async fn remembered_memory_hosts(&self, root: &Path) -> Result<HashSet<String>, String> {
        let (core, root) = (self.core.clone(), root.to_path_buf());
        rovik_core::rt::spawn_blocking(move || {
            let repo = core.memory.repo(&root);
            if !rovik_core::memory::store_path(&core.paths, Path::new(&repo)).is_file() {
                return Ok(HashSet::new());
            }
            let store = core
                .memory
                .open(&core.paths, &root)
                .ok_or("The local memory couldn't be opened.")?;
            Ok(store
                .sync_peers(&repo)
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter_map(|peer| peer.rsplit_once(':').map(|(host, _)| host.to_string()))
                .collect())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    /// Refreshes the manager's local replica and sends edits/deletions to every
    /// connected host known to hold this project. Offline hosts catch up later.
    pub async fn sync_project_memory(&self, root: &Path) -> Result<(), String> {
        let opened = self
            .lock()
            .open
            .clone()
            .filter(|(_, path)| projects::path_key(path) == projects::path_key(root));
        let project = match opened {
            Some((id, _)) => Some(id),
            None => {
                let key = projects::path_key(root);
                self.core
                    .db
                    .read(move |c| {
                        Ok(projects::list(c)?
                            .into_iter()
                            .find(|p| projects::path_key(Path::new(&p.path)) == key)
                            .map(|p| p.id))
                    })
                    .await?
            }
        };
        let Some(project) = project else {
            return Ok(());
        };
        let mut hosts: HashSet<String> = self
            .lock()
            .ensured
            .iter()
            .filter(|(_, p)| p == &project)
            .map(|(h, _)| h.clone())
            .collect();
        let chats: Vec<_> = self
            .lock()
            .chats
            .iter()
            .map(|(c, h)| (c.clone(), h.clone()))
            .collect();
        for (chat, host) in chats {
            if self
                .project_of(&chat)
                .await?
                .is_some_and(|(p, _)| p == project)
            {
                hosts.insert(host);
            }
        }
        hosts.extend(self.remembered_memory_hosts(root).await?);
        let ready: HashSet<_> = self.hosts.ready().into_iter().map(|(h, _)| h).collect();
        for host in hosts {
            if ready.contains(&host) {
                self.sync_memory(&host, &project, root).await?;
            }
        }
        Ok(())
    }
}

async fn apply(
    store: &SharedMemoryStore,
    repo: &str,
    peer: &str,
    progress: &mut Progress,
    page: Page,
) -> Result<(), String> {
    let (store, repo, peer) = (store.clone(), repo.to_string(), peer.to_string());
    let mut next = progress.clone();
    next.received = page.through;
    let saved = next.clone();
    rovik_core::rt::spawn_blocking(move || {
        store.sync_apply(&repo, &page)?;
        store.sync_ack(&repo, &peer, &saved)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    *progress = next;
    Ok(())
}
