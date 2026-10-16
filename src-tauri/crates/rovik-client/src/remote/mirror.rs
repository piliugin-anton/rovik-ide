//! The mirror of each remote chat (6.3): its rows as its host has them,
//! pulled from where the mirror is (`mirror_state.rev`), a page at a time.
//!
//! - A pull comes 500 ms after `ChatChanged` names a revision the mirror
//!   doesn't have, so more changes join it, and at once on each connection
//!   and when a run ends. There is one at a time per chat: one asked for
//!   while another goes comes after it.
//! - Each page's files come first: an attachment the rows name and this
//!   computer lacks is fetched, and one its host can't send gets a row
//!   without a file, so it never stops a pull (6.11). Then the page's rows
//!   go in in one transaction, which checks the chat is still on that host
//!   (`sync::apply_pulled`).
//! - The frontend hears of each page's rows: new ones as `MessagesAdded`,
//!   final ones that changed as a full `MessagePatched`. A chat it opened
//!   between an event and its rows ends with what the database has (K22).
//!   A row still being written isn't patched from its checkpoint: the
//!   run's own patches carry it, and are newer.
//! - "Re-sync this chat" pulls every row and replaces the mirror's, the
//!   chat's own row aside, which the input's draft hangs from (K45).

use super::{internal, CloudEvent, Remote};
use crate::client::HostState;
use rovik_core::agent_run::events::{self, AgentEvent, OutMessage};
use rovik_core::chat_db::store::{self, MessageRow};
use rovik_core::chat_db::sync::{self, Applied};
use rovik_core::chat_db::{attachments, mirror};
use rovik_protocol::sync::{ChangeSet, ChangesPage};
use rovik_protocol::{Fail, FailCode, Request};
use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

/// How long a pull waits after `ChatChanged`, for more changes to join it.
pub(super) const DEBOUNCE: Duration = Duration::from_millis(500);

/// How often a connection's first pulls are looked at, to tell when they
/// are all done.
const CAUGHT_UP_POLL: Duration = Duration::from_millis(100);

/// A chat's pulls: whether one is coming or going, whether another is
/// asked for once it's done, and whether the last one failed.
#[derive(Default)]
pub(super) struct Pulls {
    coming: bool,
    going: bool,
    again: bool,
    failed: bool,
}

impl Remote {
    /// Pulls the chat's changes from `host` after `wait`, unless a pull is
    /// coming already; one going now is followed by another.
    pub(super) fn pull_later(&self, host: &str, chat: &str, wait: Duration) {
        {
            let mut state = self.lock();
            let pulls = state.pulls.entry(chat.to_string()).or_default();
            if pulls.going {
                pulls.again = true;
                return;
            }
            if pulls.coming {
                return;
            }
            pulls.coming = true;
        }
        let (me, host, chat) = (self.me.clone(), host.to_string(), chat.to_string());
        rovik_core::rt::spawn(async move {
            tokio::time::sleep(wait).await;
            let Some(remote) = me.upgrade() else { return };
            if remote.start_pulling(&chat, true) {
                remote.keep_pulling(&host, &chat).await;
            }
        });
    }

    /// Pulls the chats mirrored from `host`, as it connects, then tells the
    /// frontend once they have all pulled (K23).
    pub(super) async fn pull_all(&self, host: &str) {
        let on = host.to_string();
        let chats = match self.core.db.read(move |c| mirror::mirrored(c, &on)).await {
            Ok(chats) => chats,
            Err(e) => {
                log::warn!("the chats mirrored from {host}: {e}");
                return;
            }
        };
        for chat in &chats {
            self.pull_later(host, chat, Duration::ZERO);
        }
        self.tell_when_caught_up(host, chats);
    }

    /// `CaughtUp` once none of `chats` has a pull coming or going, unless one
    /// failed or `host` went away meanwhile: then the next connection tells.
    fn tell_when_caught_up(&self, host: &str, chats: Vec<String>) {
        let (me, host) = (self.me.clone(), host.to_string());
        rovik_core::rt::spawn(async move {
            loop {
                let Some(remote) = me.upgrade() else { return };
                let (busy, failed) = {
                    let state = remote.lock();
                    let pulls = || chats.iter().filter_map(|chat| state.pulls.get(chat));
                    let busy = pulls().any(|p| p.coming || p.going);
                    (busy, pulls().any(|p| p.failed))
                };
                if !busy {
                    let client = remote.client(&host);
                    let ready = client.is_ok_and(|c| c.state() == HostState::Ready);
                    if ready && !failed {
                        remote.lock().caught_up.insert(host.clone());
                        if let Some(cloud) = remote.cloud() {
                            cloud(CloudEvent::CaughtUp { host });
                        }
                    }
                    return;
                }
                drop(remote);
                tokio::time::sleep(CAUGHT_UP_POLL).await;
            }
        });
    }

    /// `ChatChanged`: a pull, if the mirror is behind `rev`.
    pub(super) async fn chat_changed(&self, host: &str, chat: &str, rev: i64) {
        if self.host_of(chat).as_deref() != Some(host) {
            return;
        }
        let id = chat.to_string();
        let at = self.core.db.read(move |c| mirror::state(c, &id)).await;
        if matches!(at, Ok(Some(ref state)) if state.rev >= rev) {
            return;
        }
        self.pull_later(host, chat, DEBOUNCE);
    }

    /// Pulls the chat's changes now, all of them, once a pull going ends:
    /// a move back's last pull (6.8). One asked for meanwhile comes after.
    pub(super) async fn pull_now(&self, host: &str, chat: &str) -> Result<(), String> {
        while !self.start_pulling(chat, false) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let pulled = self.pull(host, chat).await;
        if self.pull_again(chat) {
            let (me, host, chat) = (self.me.clone(), host.to_string(), chat.to_string());
            rovik_core::rt::spawn(async move {
                if let Some(remote) = me.upgrade() {
                    remote.keep_pulling(&host, &chat).await;
                }
            });
        }
        pulled
    }

    /// The chat's pull, once it's going, and those asked for meanwhile.
    async fn keep_pulling(&self, host: &str, chat: &str) {
        loop {
            let pulled = self.pull(host, chat).await;
            if let Err(e) = &pulled {
                log::warn!("{chat}'s mirror didn't pull from {host}: {e}");
            }
            if let Some(pulls) = self.lock().pulls.get_mut(chat) {
                pulls.failed = pulled.is_err();
            }
            if !self.pull_again(chat) {
                return;
            }
        }
    }

    /// Marks the chat's pulls as going: false when they go already, and a
    /// pull that was coming (`was_coming`) follows them then.
    fn start_pulling(&self, chat: &str, was_coming: bool) -> bool {
        let mut state = self.lock();
        let pulls = state.pulls.entry(chat.to_string()).or_default();
        if was_coming {
            pulls.coming = false;
        }
        if pulls.going {
            pulls.again |= was_coming;
            return false;
        }
        pulls.going = true;
        pulls.again = false;
        true
    }

    /// A pull of the chat is over: whether another was asked for meanwhile,
    /// in which case they still go, else they're over.
    fn pull_again(&self, chat: &str) -> bool {
        let mut state = self.lock();
        let pulls = state.pulls.entry(chat.to_string()).or_default();
        let again = std::mem::take(&mut pulls.again);
        pulls.going = again;
        again
    }

    /// Pulls the chat's changes from its host, page after page.
    async fn pull(&self, host: &str, chat: &str) -> Result<(), String> {
        let id = chat.to_string();
        let mirrored = self.core.db.read(move |c| mirror::state(c, &id)).await?;
        let Some(mirrored) = mirrored.filter(|m| m.host == host) else {
            return Ok(());
        };
        let client = self.client(host).map_err(|f| f.message)?;
        let mut cursor = None;
        loop {
            let ask = Request::ChatChangesSince {
                dialog_id: chat.to_string(),
                rev: mirrored.rev,
                cursor,
            };
            let page: ChangesPage = client.request(ask).await.map_err(|f| f.message)?;
            self.fetch_files(host, files_of(&page.changes)).await;
            let next = page.next.clone();
            let (root, on) = (chat.to_string(), host.to_string());
            let applied = self
                .core
                .db
                .write(move |c| sync::apply_pulled(c, &root, &on, &page))
                .await?;
            let Some(applied) = applied else {
                // Not on that host any more.
                return Ok(());
            };
            // Chats the pull brought, a subagent's made while this computer
            // was away: routed from now on, and told of as the lead's.
            for chat in &applied.new_chats {
                self.place(chat, host);
            }
            self.note_children(host, &applied.new_chats).await;
            self.tell(applied).await;
            match next {
                Some(next) => cursor = Some(next),
                None => return Ok(()),
            }
        }
    }

    /// What a page brought, told to the frontend.
    async fn tell(&self, applied: Applied) {
        if applied.added.is_empty() && applied.changed.is_empty() {
            return;
        }
        let ids: Vec<String> = (applied.added.iter().chain(&applied.changed))
            .map(|(_, id)| id.clone())
            .collect();
        let rows = self
            .core
            .db
            .read(move |c| store::messages_by_id(c, &ids))
            .await;
        let rows: HashMap<String, MessageRow> = match rows {
            Ok(rows) => rows.into_iter().map(|r| (r.id.clone(), r)).collect(),
            Err(e) => return log::warn!("the mirror's new rows: {e}"),
        };
        let mut added: Vec<(String, Vec<OutMessage>)> = Vec::new();
        for (chat, id) in &applied.added {
            let Some(row) = rows.get(id) else { continue };
            match added.last_mut() {
                Some((last, messages)) if last == chat => messages.push(events::out_of_row(row)),
                _ => added.push((chat.clone(), vec![events::out_of_row(row)])),
            }
        }
        for (dialog_id, messages) in added {
            self.core.emit(AgentEvent::MessagesAdded {
                dialog_id,
                messages,
            });
        }
        for (chat, id) in &applied.changed {
            let Some(row) = rows.get(id).filter(|row| row.finalized) else {
                continue;
            };
            self.core.emit(AgentEvent::MessagePatched {
                dialog_id: chat.clone(),
                message_id: id.clone(),
                patch: events::full_patch(row),
            });
        }
    }

    /// The files these attachments are, those this computer lacks, fetched
    /// from `host` before the rows that name them are written: a row can't
    /// get its files later (6.1, 6.3). One the host can't send gets a row
    /// without a file, and the rows go in all the same (K44).
    pub(super) async fn fetch_files(&self, host: &str, ids: BTreeSet<String>) {
        if ids.is_empty() {
            return;
        }
        let ids: Vec<String> = ids.into_iter().collect();
        let data_dir = self.core.db.data_dir().to_path_buf();
        let asked = ids.clone();
        let missing = self
            .core
            .db
            .read(move |c| attachments::missing_files(c, &data_dir, &asked))
            .await
            .unwrap_or(ids);
        let client = self.hosts.client(host);
        for id in missing {
            let fetched = match &client {
                Some(client) => client.blob(&id).await.ok().flatten(),
                None => None,
            };
            let stored = match fetched {
                Some(bytes) if attachments::id_of(&bytes) == id => {
                    match attachments::attachment_mime(&bytes) {
                        Some(mime) => attachments::store(&self.core.db, bytes, mime).await.is_ok(),
                        None => false,
                    }
                }
                _ => false,
            };
            if !stored {
                log::warn!("{host} couldn't send the file {id}");
                let id = id.clone();
                let added = self
                    .core
                    .db
                    .write(move |c| attachments::add_unavailable(c, &id))
                    .await;
                if let Err(e) = added {
                    log::warn!("the mirror's attachments: {e}");
                }
            }
        }
    }

    /// "Re-sync this chat" (6.3): every row its host has, in place of the
    /// mirror's, in one transaction once they've all come. The chat's own
    /// row stays, with the input's draft and its files (K45). No pull goes
    /// meanwhile; one asked for comes after.
    pub async fn resync_chat(&self, dialog_id: &str) -> Result<(), Fail> {
        let host = self
            .host_of(dialog_id)
            .ok_or_else(|| Fail::new(FailCode::BadRequest, "This chat isn't on a server."))?;
        let id = dialog_id.to_string();
        let chat = self
            .core
            .db
            .read(move |c| store::lead_chat(c, &id))
            .await
            .map_err(internal)?;
        while !self.start_pulling(&chat, false) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let replaced = self.replace(&host, &chat).await;
        if self.pull_again(&chat) {
            let (me, host, chat) = (self.me.clone(), host.clone(), chat.clone());
            rovik_core::rt::spawn(async move {
                if let Some(remote) = me.upgrade() {
                    remote.keep_pulling(&host, &chat).await;
                }
            });
        }
        replaced
    }

    async fn replace(&self, host: &str, chat: &str) -> Result<(), Fail> {
        let client = self.client(host)?;
        let mut rows = ChangeSet::default();
        let mut cursor = None;
        let rev = loop {
            let ask = Request::ChatChangesSince {
                dialog_id: chat.to_string(),
                rev: 0,
                cursor,
            };
            let page: ChangesPage = client.request(ask).await?;
            self.fetch_files(host, files_of(&page.changes)).await;
            let changes = page.changes;
            rows.dialogs.extend(changes.dialogs);
            rows.clm_states.extend(changes.clm_states);
            rows.clm_revisions.extend(changes.clm_revisions);
            rows.messages.extend(changes.messages);
            rows.clm_kept = changes.clm_kept.or(rows.clm_kept);
            match page.next {
                Some(next) => cursor = Some(next),
                None => break page.rev,
            }
        };
        let (root, on) = (chat.to_string(), host.to_string());
        let replaced = self
            .core
            .db
            .write(move |c| sync::replace_mirror(c, &root, &on, &rows, rev))
            .await
            .map_err(internal)?;
        match replaced {
            Some(_) => Ok(()),
            None => Err(Fail::new(
                FailCode::BadRequest,
                "This chat isn't on that server any more.",
            )),
        }
    }
}

/// The attachments a page's messages name.
fn files_of(changes: &ChangeSet) -> BTreeSet<String> {
    let named = changes.messages.iter().flat_map(|m| m.attachments.iter());
    named.cloned().collect()
}
