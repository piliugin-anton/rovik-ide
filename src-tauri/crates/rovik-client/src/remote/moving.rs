//! Moving a chat to a host (6.8): the preflight, then the move, a step at
//! a time, each kept in the chat's `cloud_moves` row (`chat_db::moves`).
//!
//! 1. The lock: the chat is `moving`, so nothing here changes it and no
//!    run starts in it (K25).
//! 2. A run going pauses for the move (`AgentPause`). Stop still stops it,
//!    and what it waits on can still be answered.
//! 3. The project's snapshot goes up, the chat's overlay folded in (T09).
//! 4. So do the files its messages name that the host lacks.
//! 5. The import: its rows since 0, its project and its pending reason, in
//!    one bundle (M04).
//! 6. The flip: the chat is on the host, its mirror at the import's
//!    revision, and its overlay here goes.
//! 7. Its run picks up there: `ContinueRun` after a pause for the move,
//!    `Resume` for tool calls without results, with the start the app gave.
//!
//! Until the flip, a failure or "Cancel move" makes the chat local again;
//! once the bundle went, the host may have it, so it's released there,
//! then or when the host is next connected. A move cut short, the app
//! closed, is finished or undone once its host says whether it has the
//! chat from that move (`ChatOwner`).

use super::sync::{settings, Snapshot};
use super::{internal, CloudEvent, Remote};
use crate::client::{Client, HostState};
use crate::shadow::{self, Shadow};
use rovik_core::agent_run::{self, StartKind, StartRequest};
use rovik_core::chat_db::location::{self, Location};
use rovik_core::chat_db::moves::{self, CloudMove};
use rovik_core::chat_db::{attachments, projects, store, sync};
use rovik_core::clm::store as clm;
use rovik_core::overlay::chats;
use rovik_core::rt;
use rovik_protocol::host::HostInfo;
use rovik_protocol::permissions::Approver;
use rovik_protocol::sync::{ChatBundle, Ownership};
use rovik_protocol::work::Project;
use rovik_protocol::{features, Fail, FailCode, Request, PROTOCOL};
use serde::Serialize;
use std::path::PathBuf;

/// What the move dialog shows before a chat moves (6.8's preflight).
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MovePreflight {
    /// Why the chat can't move now, if it can't.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<String>,
    /// What will wait for the user there, from the permissions and the
    /// host's approval policy (6.9).
    pub waits: Vec<String>,
    /// Its CLM context becomes trimming: the host has no virtual files.
    pub trimming: bool,
    /// What the project's push carries that the user hears of first (6.4).
    pub notes: Vec<String>,
    /// A parallel group's "Redo sequentially" won't be there after.
    pub redo_lost: bool,
    /// The files its messages name that the host lacks, and their size.
    pub files: usize,
    pub file_bytes: u64,
    /// The host's Rovik is too old to take the chat, and the user is one
    /// of its administrators there, who upgrade it for everyone (R02).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub upgrade: bool,
}

impl Remote {
    /// What moving the chat to `host` would do, and whether it can.
    pub async fn move_preflight(&self, dialog_id: &str, host: &str) -> Result<MovePreflight, Fail> {
        let blocked = |why: String| MovePreflight {
            blocked: Some(why),
            ..MovePreflight::default()
        };
        let client = match self.ready(host) {
            Ok(client) => client,
            Err(fail) if fail.code == FailCode::Version => return Ok(self.too_old(host).await),
            Err(fail) => return Ok(blocked(fail.message)),
        };
        let (project, root) = match self.movable(dialog_id).await {
            Ok(place) => place,
            Err(fail) => return Ok(blocked(fail.message)),
        };
        let mut preflight = MovePreflight::default();

        let mut config = self.core.agents.config();
        self.policy(host).apply(&mut config);
        let p = &config.permissions;
        for (rule, what) in [
            (p.command_approval, "Commands"),
            (p.edit_approval, "Edits"),
            (p.tool_approval, "Other tool calls"),
        ] {
            if rule.required && rule.by == Approver::User {
                preflight
                    .waits
                    .push(format!("{what} wait there for you to approve them."));
            }
        }

        let info: HostInfo = client.request(Request::HostInfo).await?;
        let id = dialog_id.to_string();
        let mode = self.core.db.read(move |c| clm::context_mode(c, &id));
        let clm = mode.await.map_err(internal)?.as_deref() == Some("clm");
        preflight.trimming = clm && !info.virtual_files;

        let shadow_dir = shadow::dir(&self.core.paths.data_dir, &project);
        let chat = dialog_id.to_string();
        let guards = rt::spawn_blocking(move || {
            let shadow = Shadow::open(&shadow_dir, &root)?;
            shadow.guards(&shadow.snapshot(&chat)?)
        });
        let guards = guards.await.map_err(|e| internal(e.to_string()))?;
        let guards = guards.map_err(internal)?;
        let mb = |bytes: u64| bytes.div_ceil(1 << 20);
        for (path, bytes) in &guards.large_files {
            let note = format!("{path} ({} MB) goes up with the project.", mb(*bytes));
            preflight.notes.push(note);
        }
        for (path, bytes) in &guards.lfs {
            let note = format!(
                "{path} is a Git LFS file: its {} MB go up whole.",
                mb(*bytes)
            );
            preflight.notes.push(note);
        }
        for path in &guards.nested {
            let note = format!("{path} is a repository of its own: it stays behind.");
            preflight.notes.push(note);
        }
        for path in &guards.crlf_scripts {
            let note = format!("{path} has Windows line endings: it won't run on the server.");
            preflight.notes.push(note);
        }

        let status = chats::overlay_status(dialog_id.to_string()).await;
        preflight.redo_lost = status.is_ok_and(|s| !s.redo_groups.is_empty());

        let (ids, sizes) = self.files_to_send(&client, dialog_id).await?;
        preflight.files = ids.len();
        preflight.file_bytes = sizes;
        Ok(preflight)
    }

    /// Moves the chat to `host` (6.8). `start` is how the app would start
    /// it: once it's there, its run picks up with it, if it paused for the
    /// move or waits for tool calls' results. Progress goes on
    /// `cloud-event`.
    pub async fn move_chat(
        &self,
        dialog_id: &str,
        host: &str,
        start: Option<StartRequest>,
    ) -> Result<(), Fail> {
        let client = self.ready(host)?;
        let (project, root) = self.movable(dialog_id).await?;
        // Before its row, which a connection's settling then leaves alone.
        {
            let mut state = self.lock();
            if !state.moving.insert(dialog_id.to_string()) {
                return Err(Fail::new(
                    FailCode::BadRequest,
                    format!("This chat is moving to {host} already."),
                ));
            }
            state.cancels.remove(dialog_id);
        }
        let move_id = uuid::Uuid::new_v4().to_string();
        let (chat, to, id) = (dialog_id.to_string(), host.to_string(), move_id.clone());
        let locked = self.core.db.write(move |c| moves::lock(c, &chat, &to, &id));
        if let Err(e) = locked.await {
            self.lock().moving.remove(dialog_id);
            return Err(Fail::new(FailCode::BadRequest, e));
        }
        let place = (project.as_str(), root);
        let moved = self
            .carry_over(&client, dialog_id, host, &move_id, place)
            .await;
        {
            let mut state = self.lock();
            state.moving.remove(dialog_id);
            state.cancels.remove(dialog_id);
        }
        let paused = match moved {
            Ok(paused) => paused,
            Err((fail, sent)) => {
                self.undo_move(&client, dialog_id, &move_id, sent).await;
                let step = if fail.message == CANCELLED {
                    "cancelled"
                } else {
                    "failed"
                };
                self.moving(dialog_id, step, Some(fail.message.clone()));
                return Err(fail);
            }
        };
        self.moving(dialog_id, "moved", None);
        let Some(mut start) = start else {
            return Ok(());
        };
        let waits = agent_run::waits_for_results(&self.core, dialog_id).await;
        start.start = match (paused, waits) {
            (true, _) => StartKind::ContinueRun,
            (false, Ok(true)) => StartKind::Resume,
            _ => return Ok(()),
        };
        start.dialog_id = dialog_id.to_string();
        if let Err(fail) = self.start(start).await {
            log::warn!("{dialog_id} moved, but its run didn't pick up there: {fail}");
        }
        Ok(())
    }

    /// "Cancel move": the chat is where it was. A move going now stops at
    /// its next step; one cut short, whose host can't be asked, is undone
    /// here, with a release queued for the host if it may have the chat. A
    /// move back can't be cancelled once its host released the chat: it's
    /// finished when the host is next connected.
    pub async fn cancel_move(&self, dialog_id: &str) -> Result<(), Fail> {
        let id = dialog_id.to_string();
        let going = self.core.db.read(move |c| moves::of_chat(c, &id));
        let going = going.await.map_err(internal)?;
        let back = going.as_ref().filter(|m| m.direction == moves::BACK);
        if let Some(back) = back.filter(|m| m.step == moves::RELEASED) {
            return Err(Fail::new(
                FailCode::BadRequest,
                format!(
                    "This chat's move back is past its last step on {}: it can't be cancelled.",
                    back.host
                ),
            ));
        }
        {
            let mut state = self.lock();
            if state.moving.contains(dialog_id) {
                state.cancels.insert(dialog_id.to_string());
                return Ok(());
            }
        }
        let Some(going) = going else {
            return Ok(());
        };
        let id = dialog_id.to_string();
        if going.direction == moves::BACK {
            // Before the release the chat is still there, as it was; once
            // here, its host keeps its rows until its own cleanup.
            let dropped = self.core.db.write(move |c| moves::forget(c, &id));
            dropped.await.map_err(internal)?;
            if going.step != moves::FORGETTING {
                if let Some(cloud) = self.cloud() {
                    cloud(CloudEvent::MovingBack {
                        dialog_id: dialog_id.to_string(),
                        step: "cancelled".into(),
                        message: None,
                    });
                }
            }
            return Ok(());
        }
        let undone = match going.step.as_str() {
            moves::CANCELLED => return Ok(()),
            moves::IMPORTING => self.core.db.write(move |c| moves::cancel(c, &id)).await,
            _ => self.core.db.write(move |c| moves::roll_back(c, &id)).await,
        };
        undone.map_err(internal)?;
        self.moving(dialog_id, "cancelled", None);
        Ok(())
    }

    /// The moves to and from `host` that the app left unfinished, once it's
    /// connected: a cancelled one's release goes; one cut short is finished
    /// if the host has the chat from that move, and undone if not. A move
    /// back is settled as remote/back.rs says.
    pub(super) async fn finish_moves(&self, host: &str) {
        let on = host.to_string();
        let left = self.core.db.read(move |c| moves::with_host(c, &on)).await;
        let left = match left {
            Ok(left) => left,
            Err(e) => return log::warn!("the moves to {host}: {e}"),
        };
        let Ok(client) = self.client(host) else {
            return;
        };
        for going in left {
            if self.lock().moving.contains(&going.dialog_id) {
                continue;
            }
            let finished = match going.direction.as_str() {
                moves::BACK => self.finish_back_cut_short(&client, &going).await,
                _ => self.finish_move(&client, &going).await,
            };
            if let Err(fail) = finished {
                log::warn!("{}'s move with {host} waits: {fail}", going.dialog_id);
            }
        }
    }

    async fn finish_move(&self, client: &Client, going: &CloudMove) -> Result<(), Fail> {
        let chat = going.dialog_id.clone();
        if going.step == moves::CANCELLED {
            let release = Request::ChatRelease {
                dialog_id: chat.clone(),
                move_id: Some(going.move_id.clone()),
            };
            match client.request::<bool>(release).await {
                Ok(true) => forget_cancelled(client, &chat).await,
                Ok(false) => {}
                Err(fail) if fail.code == FailCode::NotOwner => {}
                Err(fail) => return Err(fail),
            }
            let id = chat.clone();
            let forgotten = self.core.db.write(move |c| moves::forget(c, &id));
            return forgotten.await.map_err(internal);
        }
        let asked = Request::ChatOwner {
            dialog_id: chat.clone(),
        };
        let owned = match client.request::<Option<Ownership>>(asked).await {
            Ok(owned) => owned,
            Err(fail) if fail.code == FailCode::NotOwner => None,
            Err(fail) => return Err(fail),
        };
        match owned {
            Some(owned) if owned.move_id.as_ref() == Some(&going.move_id) && !owned.released => {
                self.flip(&chat, &going.host, owned.rev).await?;
                self.moving(&chat, "moved", None);
            }
            _ => {
                let id = chat.clone();
                let undone = self.core.db.write(move |c| moves::roll_back(c, &id));
                undone.await.map_err(internal)?;
                self.moving(&chat, "cancelled", None);
            }
        }
        Ok(())
    }

    /// Steps 2 to 6. Returns whether the run paused for the move; a failure
    /// says whether the bundle went, after which the host may have it.
    async fn carry_over(
        &self,
        client: &Client,
        dialog_id: &str,
        host: &str,
        move_id: &str,
        (project, root): (&str, PathBuf),
    ) -> Result<bool, (Fail, bool)> {
        let before = |fail: Fail| (fail, false);
        let mut paused = false;
        if self.core.agents.is_running(dialog_id) {
            self.moving(dialog_id, "pausing", None);
            let pause = agent_run::agent_pause(&self.core, dialog_id.into(), host.into());
            paused = pause.await.map_err(|e| before(internal(e)))?;
        }
        self.step(dialog_id, moves::PAUSED).await.map_err(before)?;
        self.not_cancelled(dialog_id).map_err(before)?;

        self.moving(dialog_id, "pushing", None);
        let shadow_dir = shadow::dir(&self.core.paths.data_dir, project);
        let (at, folder, chat) = (shadow_dir.clone(), root.clone(), dialog_id.to_string());
        let folded = rt::spawn_blocking(move || {
            let shadow = Shadow::open(&at, &folder)?;
            let folded = shadow.fold(&chat, &chats::changes(&chat))?;
            Ok::<_, String>((folded, shadow.excludes(), shadow.subpath().to_string()))
        });
        let folded = folded.await.map_err(|e| before(internal(e.to_string())))?;
        let (folded, excludes, subpath) = folded.map_err(|e| before(internal(e)))?;
        let snapshot = Snapshot {
            project: project.to_string(),
            root: root.clone(),
            shadow: shadow_dir,
            commit: folded.start,
            excludes: excludes.clone(),
            subpath: subpath.clone(),
        };
        let carried = self.carry(host, client, dialog_id, &snapshot).await;
        carried.map_err(|e| before(Fail::new(FailCode::Unreachable, e)))?;
        self.step(dialog_id, moves::PUSHED).await.map_err(before)?;
        self.not_cancelled(dialog_id).map_err(before)?;

        self.moving(dialog_id, "uploading", None);
        self.send_files_of(client, dialog_id)
            .await
            .map_err(before)?;
        self.step(dialog_id, moves::UPLOADED)
            .await
            .map_err(before)?;
        self.not_cancelled(dialog_id).map_err(before)?;

        self.moving(dialog_id, "importing", None);
        let id = dialog_id.to_string();
        let rows = self
            .core
            .db
            .read(move |c| sync::changes_since(c, &id, 0, None, usize::MAX))
            .await
            .map_err(|e| before(internal(e)))?
            .changes;
        let id = project.to_string();
        let summary = self.core.db.read(move |c| projects::summary(c, &id));
        let name = summary.await.map_err(|e| before(internal(e)))?;
        let chat = dialog_id.to_string();
        let pending = rt::spawn_blocking(move || chats::pending(&chat).map(|p| p.reason));
        let bundle = ChatBundle {
            dialog_id: dialog_id.to_string(),
            rows,
            project: Project {
                id: project.to_string(),
                name: name.map(|s| s.name).unwrap_or_default(),
                subpath,
            },
            settings: settings(&self.core.paths.config_dir, &root),
            excludes,
            snapshot: folded.base.to_string(),
            changes: Some(folded.start.to_string()),
            pending: pending.await.unwrap_or(None),
        };
        // From here on the host may have the chat.
        self.step(dialog_id, moves::IMPORTING)
            .await
            .map_err(before)?;
        let after = |fail: Fail| (fail, true);
        let imported = client.import_chat(move_id, &bundle, |_| {}).await;
        let imported = imported.map_err(after)?;
        self.not_cancelled(dialog_id).map_err(after)?;
        self.flip(dialog_id, host, imported.rev)
            .await
            .map_err(after)?;
        Ok(paused)
    }

    /// The flip (step 6), here and in the database, and the chat's overlay
    /// here gone: its changes are its worktree's on the host.
    async fn flip(&self, dialog_id: &str, host: &str, rev: i64) -> Result<(), Fail> {
        let (chat, on) = (dialog_id.to_string(), host.to_string());
        let flipped = self.core.db.write(move |c| {
            moves::flip(c, &chat, &on, rev)?;
            let mut chats = store::descendants(c, &chat)?;
            chats.push(chat);
            Ok(chats)
        });
        let chats = flipped.await.map_err(internal)?;
        {
            let mut state = self.lock();
            for chat in chats {
                state.chats.insert(chat, host.to_string());
            }
        }
        let chat = dialog_id.to_string();
        let discarded = rt::spawn_blocking(move || {
            if chats::exists(&chat) {
                chats::discard(&chat)
            } else {
                Ok(())
            }
        });
        match discarded.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => log::warn!("{dialog_id}'s overlay stays here: {e}"),
            Err(e) => log::warn!("{dialog_id}'s overlay stays here: {e}"),
        }
        self.changed(dialog_id);
        Ok(())
    }

    /// A move that failed or was cancelled before its flip: the chat is
    /// here again. Once its bundle went, the host may have it: it's
    /// released there now, or when the host is next connected.
    async fn undo_move(&self, client: &Client, dialog_id: &str, move_id: &str, sent: bool) {
        let mut released = !sent;
        if sent {
            let release = Request::ChatRelease {
                dialog_id: dialog_id.to_string(),
                move_id: Some(move_id.to_string()),
            };
            let answer = client.request::<bool>(release).await;
            if answer == Ok(true) {
                forget_cancelled(client, dialog_id).await;
            }
            released = answer.is_ok();
        }
        let id = dialog_id.to_string();
        let undone = match released {
            true => self.core.db.write(move |c| moves::roll_back(c, &id)).await,
            false => self.core.db.write(move |c| moves::cancel(c, &id)).await,
        };
        if let Err(e) = undone {
            log::error!("{dialog_id} stays locked: {e}");
        }
    }

    /// The files the chat's messages name that the host lacks go up, with
    /// their progress on `cloud-event`. One this computer has lost goes as
    /// a row without a file (6.11).
    async fn send_files_of(&self, client: &Client, dialog_id: &str) -> Result<(), Fail> {
        let (missing, _) = self.files_to_send(client, dialog_id).await?;
        let data_dir = self.core.db.data_dir().to_path_buf();
        let total = missing.len();
        for (n, id) in missing.into_iter().enumerate() {
            let Some(bytes) =
                attachments::file_for(&data_dir, &id).and_then(|f| std::fs::read(f).ok())
            else {
                continue;
            };
            let (cloud, chat) = (self.cloud(), dialog_id.to_string());
            let progress = move |_| {
                if let Some(cloud) = &cloud {
                    cloud(CloudEvent::Moving {
                        dialog_id: chat.clone(),
                        step: "uploading".into(),
                        message: Some(format!("File {} of {total}", n + 1)),
                    });
                }
            };
            client.put_blob(&id, bytes, progress).await?;
        }
        Ok(())
    }

    /// The files the chat's messages name that this computer has and the
    /// host doesn't, and their size.
    async fn files_to_send(
        &self,
        client: &Client,
        dialog_id: &str,
    ) -> Result<(Vec<String>, u64), Fail> {
        let id = dialog_id.to_string();
        let named = self.core.db.read(move |c| attachments::of_subtree(c, &id));
        let data_dir = self.core.db.data_dir().to_path_buf();
        let here: Vec<String> = named
            .await
            .map_err(internal)?
            .into_iter()
            .filter(|id| attachments::file_for(&data_dir, id).is_some())
            .collect();
        if here.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let missing: Vec<String> = client.request(Request::BlobsMissing { ids: here }).await?;
        let size = |id: &String| {
            let file = attachments::file_for(&data_dir, id);
            file.and_then(|f| std::fs::metadata(f).ok())
                .map_or(0, |m| m.len())
        };
        let bytes = missing.iter().map(size).sum();
        Ok((missing, bytes))
    }

    /// The chat can move: it's here, has a project with a folder here, and
    /// no conflicts wait in its overlay. Its project and folder.
    async fn movable(&self, dialog_id: &str) -> Result<(String, PathBuf), Fail> {
        let refused = |why: &str| Fail::new(FailCode::BadRequest, why);
        let id = dialog_id.to_string();
        let at = self.core.db.read(move |c| location::of_chat(c, &id)).await;
        match at.map_err(internal)? {
            Location::Here => {}
            Location::Moving(host) => {
                return Err(refused(&format!("This chat is moving to {host} already.")))
            }
            Location::Host(host) => {
                return Err(refused(&format!("This chat is on {host} already.")))
            }
            Location::Released => return Err(refused("This chat went back to its computer.")),
        }
        let id = dialog_id.to_string();
        let lead = self.core.db.read(move |c| store::lead_chat(c, &id)).await;
        if lead.map_err(internal)? != dialog_id {
            return Err(refused("A subagent's chat moves with its lead's."));
        }
        let place = self.project_of(dialog_id).await.map_err(internal)?;
        let place = place.ok_or_else(|| {
            refused(
                "Only a chat of a project with its folder on this computer can move to a server.",
            )
        })?;
        let chat = dialog_id.to_string();
        let pending = rt::spawn_blocking(move || chats::pending(&chat));
        let conflicts = pending
            .await
            .ok()
            .flatten()
            .map_or(0, |p| p.conflicts.len());
        if conflicts > 0 {
            let files = if conflicts == 1 { "file" } else { "files" };
            return Err(refused(&format!(
                "{conflicts} {files} of this chat conflict with the project's changes: settle them first."
            )));
        }
        Ok(place)
    }

    /// A host whose Rovik is older than this app's, which can't take chats
    /// (4.5, R02): an administrator there upgrades it for everyone, and
    /// anyone else is told whom to ask.
    async fn too_old(&self, host: &str) -> MovePreflight {
        let name = self.host_name(host);
        let info: Option<HostInfo> = match self.client(host) {
            Ok(client) => client.request(Request::HostInfo).await.ok(),
            Err(_) => None,
        };
        let version = info.as_ref().map(|i| i.version.as_str());
        let runs = match version.filter(|v| !v.is_empty()) {
            Some(version) => format!("{name} runs Rovik {version}"),
            None => format!("{name} runs an older Rovik"),
        };
        let admins = info
            .as_ref()
            .and_then(|i| i.install.as_ref())
            .map_or_else(Vec::new, |i| i.admins.clone());
        let upgrade = info.as_ref().is_some_and(|i| admins.contains(&i.user));
        let blocked = match admins.as_slice() {
            _ if upgrade => {
                format!("{runs}, which can't take chats: upgrade it for everyone first.")
            }
            [] => format!("{runs}, which can't take chats: it needs an upgrade first."),
            admins => format!(
                "{runs}, which can't take chats: ask {} to upgrade it.",
                admins.join(" or ")
            ),
        };
        MovePreflight {
            blocked: Some(blocked),
            upgrade,
            ..MovePreflight::default()
        }
    }

    /// `host`'s connection, if it can take a chat: ready, and its Rovik as
    /// new as this app's (4.5).
    pub(super) fn ready(&self, host: &str) -> Result<Client, Fail> {
        let client = self.client(host)?;
        if client.state() != HostState::Ready {
            return Err(Fail::new(
                FailCode::Unreachable,
                format!("{host} isn't connected."),
            ));
        }
        let takes = client.daemon().is_some_and(|d| {
            d.protocol >= PROTOCOL && d.features.iter().any(|f| f == features::MIRROR)
        });
        if !takes {
            let name = self.host_name(host);
            return Err(Fail::new(
                FailCode::Version,
                format!("{name} runs an older Rovik, which can't take chats: upgrade it first."),
            ));
        }
        Ok(client)
    }

    pub(super) async fn step(&self, dialog_id: &str, step: &'static str) -> Result<(), Fail> {
        let id = dialog_id.to_string();
        let stepped = self.core.db.write(move |c| moves::step(c, &id, step));
        stepped.await.map_err(internal)
    }

    /// Fails once "Cancel move" was asked.
    pub(super) fn not_cancelled(&self, dialog_id: &str) -> Result<(), Fail> {
        match self.lock().cancels.contains(dialog_id) {
            true => Err(Fail::new(FailCode::BadRequest, CANCELLED)),
            false => Ok(()),
        }
    }

    fn moving(&self, dialog_id: &str, step: &str, message: Option<String>) {
        if let Some(cloud) = self.cloud() {
            cloud(CloudEvent::Moving {
                dialog_id: dialog_id.to_string(),
                step: step.to_string(),
                message,
            });
        }
    }
}

/// What a cancelled move fails with.
pub(super) const CANCELLED: &str = "The move was cancelled.";

/// The rows a move left on its host once it was cancelled there go too:
/// this computer has the chat. Should the host not hear it, its cleanup
/// takes them (R01).
async fn forget_cancelled(client: &Client, dialog_id: &str) {
    let forget = Request::ChatForget {
        dialog_id: dialog_id.to_string(),
    };
    if let Err(fail) = client.request::<bool>(forget).await {
        log::warn!("{dialog_id}'s rows stay on its host until its cleanup: {fail}");
    }
}
