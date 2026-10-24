//! The project transport's packs over HTTP (4.4, 6.4): the tips a daemon
//! has for a project, the ref a pushed pack moves, and what a fetch asks
//! for. The packs themselves are git's own format.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Where a client's refs go: a daemon moves no other ref for a pack.
pub const PUSHED_REFS: &str = "refs/rovik/";

/// Where a chat's snapshot goes on its host (6.4).
pub fn snapshot_ref(chat: &str) -> String {
    format!("{PUSHED_REFS}chats/{chat}/snapshot")
}

/// Where the daemon commits a chat's result (6.5).
pub fn result_ref(chat: &str) -> String {
    format!("{PUSHED_REFS}chats/{chat}/result")
}

/// Where the daemon keeps what Undo the merge puts back in a chat's files
/// (6.1), so that no `git gc` prunes it.
pub fn kept_ref(chat: &str) -> String {
    format!("{PUSHED_REFS}chats/{chat}/kept")
}

/// `GET /packs/<project>/refs`: each ref the daemon has for the project,
/// with the commit it points at.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Refs {
    pub refs: BTreeMap<String, String>,
}

/// `POST /packs/<project>/fetch`: the commits a client wants, and those it
/// has, so that the pack holds only what it lacks.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Fetch {
    pub wants: Vec<String>,
    pub haves: Vec<String>,
}

/// What a pushed pack brought: how many objects.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Pushed {
    pub objects: usize,
}

/// `POST /packs/<project>?ref=<name>&old=<oid>&new=<oid>`: the ref a pushed
/// pack moves, from `old` to `new`. Without `old` the ref must be new.
#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    pub name: String,
    pub old: Option<String>,
    pub new: String,
}

impl Update {
    /// The query string, without its `?`. Names and ids need no escaping:
    /// `parse` takes nothing else.
    pub fn query(&self) -> String {
        let old = self.old.as_deref().unwrap_or_default();
        format!("ref={}&old={old}&new={}", self.name, self.new)
    }

    /// The update a query string names, when its ref is one a client may
    /// move and its ids are commit ids.
    pub fn parse(query: &str) -> Option<Update> {
        let mut fields: BTreeMap<&str, &str> = BTreeMap::new();
        for pair in query.split('&') {
            let (key, value) = pair.split_once('=')?;
            fields.insert(key, value);
        }
        let name = (*fields.get("ref")?).to_string();
        let new = (*fields.get("new")?).to_string();
        let old = fields.get("old").filter(|old| !old.is_empty());
        let old = old.map(|old| old.to_string());
        let fine = pushable(&name) && is_oid(&new) && old.as_deref().is_none_or(is_oid);
        fine.then_some(Update { name, old, new })
    }
}

/// Whether a client may push to `name`: a chat's snapshot ref
/// ([`snapshot_ref`]), the only one a client moves (the others under
/// `PUSHED_REFS` are the daemon's), its chat one part of letters, digits,
/// `-`, `_` and `.`, not dotted at either end. So every pushed ref names
/// its chat, whose owner the daemon checks: a malformed one
/// (`refs/rovik/chats/<chat>` alone) would skip that check and, created,
/// block every chat's snapshot ref under it.
pub fn pushable(name: &str) -> bool {
    let Some(chat) = name
        .strip_prefix(PUSHED_REFS)
        .and_then(|rest| rest.strip_prefix("chats/"))
        .and_then(|rest| rest.strip_suffix("/snapshot"))
    else {
        return false;
    };
    let plain = |c: char| c.is_ascii_alphanumeric() || "-_.".contains(c);
    !chat.is_empty()
        && chat.chars().all(plain)
        && !chat.starts_with('.')
        && !chat.ends_with('.')
        && !chat.ends_with(".lock")
}

/// Whether `id` is a commit id as git writes it: 40 hex digits.
pub fn is_oid(id: &str) -> bool {
    id.len() == 40 && id.bytes().all(|b| b.is_ascii_hexdigit())
}
