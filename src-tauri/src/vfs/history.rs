//! Revisions of virtual files.
//!
//! A revision is the whole content of a file at the moment a change was
//! finished: when a program closes a file it modified, or when the app
//! writes one. (Not per write request — a single `cat > f` makes dozens.)
//!
//! History belongs to the file, not the path:
//! - a rename keeps it;
//! - "safe saves" (write a temporary file, rename it over the original, as
//!   `sed -i` and most editors do) continue the original's history: the
//!   replaced file's revisions come first, the new file's follow;
//! - a deleted file's history stays, found by the path it last had, until
//!   it's evicted.
//!
//! Memory is bounded: at most `per_file` revisions per file and `max_bytes`
//! in all, dropping the oldest revisions first; files over `max_file_size`
//! aren't versioned; content equal to the latest revision isn't stored
//! again. Revisions are immutable and shared (`Arc`), so reading one doesn't
//! copy it under the filesystem's lock.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::events::Source;
use super::store::Ino;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryLimits {
    /// All revisions together; 0 turns versioning off.
    pub max_bytes: u64,
    /// Revisions kept per file.
    pub per_file: usize,
    /// Larger files aren't versioned.
    pub max_file_size: u64,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        HistoryLimits { max_bytes: 256 << 20, per_file: 64, max_file_size: 32 << 20 }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionInfo {
    /// 1, 2, 3… per file (older ones may have been dropped).
    pub number: u64,
    pub size: u64,
    /// Milliseconds since the Unix epoch.
    pub time: u64,
    /// Who made the change.
    pub source: Source,
}

struct Revision {
    /// Global and increasing: eviction drops the smallest first.
    id: u64,
    number: u64,
    data: Arc<[u8]>,
    time: SystemTime,
    source: Source,
}

impl Revision {
    fn info(&self) -> RevisionInfo {
        RevisionInfo {
            number: self.number,
            size: self.data.len() as u64,
            time: self.time.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
            source: self.source,
        }
    }
}

struct FileHistory {
    revisions: VecDeque<Revision>,
    next_number: u64,
    /// For a deleted file: the path it last had.
    path: String,
    deleted: bool,
}

pub(crate) struct Histories {
    files: HashMap<Ino, FileHistory>,
    /// Revision id → file, oldest first.
    order: BTreeMap<u64, Ino>,
    bytes: u64,
    next_id: u64,
    limits: HistoryLimits,
}

impl Histories {
    pub fn new(limits: HistoryLimits) -> Self {
        Histories { files: HashMap::new(), order: BTreeMap::new(), bytes: 0, next_id: 1, limits }
    }

    pub fn limits(&self) -> HistoryLimits {
        self.limits
    }

    pub fn set_limits(&mut self, limits: HistoryLimits) {
        self.limits = limits;
        let files: Vec<Ino> = self.files.keys().copied().collect();
        for ino in files {
            self.trim_file(ino);
        }
        self.evict();
    }

    /// Whether content of this size would be recorded (callers skip the
    /// copy otherwise).
    pub fn accepts(&self, size: usize) -> bool {
        self.limits.max_bytes > 0 && self.limits.per_file > 0 && size as u64 <= self.limits.max_file_size
    }

    /// Records `data` as the file's newest revision, unless it's the same
    /// as the current newest. Returns the revision's number.
    pub fn record(&mut self, ino: Ino, path: &str, data: &[u8], source: Source) -> Option<u64> {
        if !self.accepts(data.len()) {
            return None;
        }
        let history = self.files.entry(ino).or_insert_with(|| FileHistory {
            revisions: VecDeque::new(),
            next_number: 1,
            path: String::new(),
            deleted: false,
        });
        history.path = path.to_string();
        history.deleted = false;
        if history.revisions.back().is_some_and(|last| &*last.data == data) {
            return None;
        }
        let (id, number) = (self.next_id, history.next_number);
        self.next_id += 1;
        history.next_number += 1;
        history.revisions.push_back(Revision { id, number, data: Arc::from(data), time: SystemTime::now(), source });
        self.order.insert(id, ino);
        self.bytes += data.len() as u64;
        self.trim_file(ino);
        self.evict();
        Some(number)
    }

    fn remove_revision(&mut self, ino: Ino, id: u64) {
        self.order.remove(&id);
        let Some(history) = self.files.get_mut(&ino) else { return };
        if let Some(at) = history.revisions.iter().position(|r| r.id == id) {
            if let Some(rev) = history.revisions.remove(at) {
                self.bytes -= rev.data.len() as u64;
            }
        }
        if history.revisions.is_empty() && history.deleted {
            self.files.remove(&ino);
        }
    }

    fn trim_file(&mut self, ino: Ino) {
        loop {
            let oldest = match self.files.get(&ino) {
                Some(h) if h.revisions.len() > self.limits.per_file => h.revisions.front().map(|r| r.id),
                _ => None,
            };
            match oldest {
                Some(id) => self.remove_revision(ino, id),
                None => break,
            }
        }
    }

    fn evict(&mut self) {
        while self.bytes > self.limits.max_bytes {
            let Some((&id, &ino)) = self.order.iter().next() else { break };
            self.remove_revision(ino, id);
        }
    }

    pub fn list(&self, ino: Ino) -> Vec<RevisionInfo> {
        self.files.get(&ino).map(|h| h.revisions.iter().map(Revision::info).collect()).unwrap_or_default()
    }

    pub fn get(&self, ino: Ino, number: u64) -> Option<Arc<[u8]>> {
        self.files.get(&ino)?.revisions.iter().find(|r| r.number == number).map(|r| r.data.clone())
    }

    pub fn contains(&self, ino: Ino) -> bool {
        self.files.contains_key(&ino)
    }

    /// The file was freed; its history stays, found by `path`.
    pub fn mark_deleted(&mut self, ino: Ino, path: &str) {
        if let Some(history) = self.files.get_mut(&ino) {
            history.deleted = true;
            history.path = path.to_string();
            if history.revisions.is_empty() {
                self.files.remove(&ino);
            }
        }
    }

    /// The most recently changed deleted file that was at `path`.
    pub fn find_deleted(&self, path: &str, case_insensitive: bool) -> Option<Ino> {
        let same = |p: &str| if case_insensitive { p.to_lowercase() == path.to_lowercase() } else { p == path };
        self.files
            .iter()
            .filter(|(_, h)| h.deleted && same(&h.path))
            .max_by_key(|(_, h)| h.revisions.back().map_or(0, |r| r.id))
            .map(|(ino, _)| *ino)
    }

    /// `replaced`'s history becomes the start of `into`'s: a file renamed
    /// over another continues its history. `into`'s own revisions are
    /// renumbered after it.
    pub fn merge_into(&mut self, replaced: Ino, into: Ino) {
        if replaced == into {
            return;
        }
        let Some(mut base) = self.files.remove(&replaced) else { return };
        if let Some(own) = self.files.remove(&into) {
            for mut rev in own.revisions {
                if base.revisions.back().is_some_and(|last| last.data == rev.data) {
                    // Nothing new: drop the duplicate.
                    self.order.remove(&rev.id);
                    self.bytes -= rev.data.len() as u64;
                    continue;
                }
                rev.number = base.next_number;
                base.next_number += 1;
                base.revisions.push_back(rev);
            }
            base.path = own.path;
        }
        base.deleted = false;
        for rev in &base.revisions {
            self.order.insert(rev.id, into);
        }
        self.files.insert(into, base);
        self.trim_file(into);
    }

    /// (revisions, bytes) held.
    pub fn usage(&self) -> (usize, u64) {
        (self.order.len(), self.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(max_bytes: u64, per_file: usize) -> Histories {
        Histories::new(HistoryLimits { max_bytes, per_file, max_file_size: 100 })
    }

    fn numbers(h: &Histories, ino: Ino) -> Vec<u64> {
        h.list(ino).into_iter().map(|r| r.number).collect()
    }

    #[test]
    fn records_and_skips_duplicates() {
        let mut h = h(1000, 10);
        assert_eq!(h.record(2, "a", b"one", Source::App), Some(1));
        assert_eq!(h.record(2, "a", b"one", Source::Process), None);
        assert_eq!(h.record(2, "a", b"two", Source::Process), Some(2));
        assert_eq!(&*h.get(2, 1).unwrap(), b"one");
        assert_eq!(h.list(2)[1].source, Source::Process);
        assert_eq!(h.usage(), (2, 6));
        assert!(h.get(2, 3).is_none());
    }

    #[test]
    fn keeps_limits() {
        let mut h = h(10, 3);
        for (i, data) in [b"aa", b"bb", b"cc", b"dd"].iter().enumerate() {
            h.record(2, "a", *data, Source::App);
            assert!(h.list(2).len() <= 3, "after {i}");
        }
        assert_eq!(numbers(&h, 2), [2, 3, 4], "oldest dropped first");
        h.record(3, "b", b"eeeeee", Source::App);
        assert!(h.usage().1 <= 10);
        assert_eq!(numbers(&h, 2), [3, 4], "global budget evicts the oldest, across files");
        assert_eq!(numbers(&h, 3), [1]);
        assert_eq!(h.record(4, "big", &[0; 101], Source::App), None, "too large to version");
        h.set_limits(HistoryLimits { max_bytes: 0, per_file: 3, max_file_size: 100 });
        assert_eq!(h.usage(), (0, 0));
        assert!(!h.accepts(1));
    }

    #[test]
    fn deleted_files_are_found_by_path() {
        let mut h = h(1000, 10);
        h.record(2, "dir/a", b"old", Source::Process);
        h.mark_deleted(2, "dir/a");
        h.record(3, "dir/a", b"newer", Source::Process);
        h.mark_deleted(3, "dir/a");
        assert_eq!(h.find_deleted("dir/a", false), Some(3), "the latest");
        assert_eq!(h.find_deleted("DIR/A", true), Some(3));
        assert_eq!(h.find_deleted("DIR/A", false), None);
        h.mark_deleted(9, "never/versioned");
        assert_eq!(h.find_deleted("never/versioned", false), None);
    }

    #[test]
    fn a_replacing_file_continues_the_history() {
        let mut h = h(1000, 10);
        h.record(2, "f", b"v1", Source::App);
        h.record(2, "f", b"v2", Source::Process);
        h.record(7, ".f.tmp", b"v3", Source::Process);
        h.merge_into(2, 7);
        assert_eq!(numbers(&h, 7), [1, 2, 3]);
        assert_eq!(&*h.get(7, 3).unwrap(), b"v3");
        assert!(h.list(2).is_empty());
        assert_eq!(h.record(7, "f", b"v4", Source::Process), Some(4));
        // Eviction still finds the moved revisions.
        h.set_limits(HistoryLimits { max_bytes: 4, per_file: 10, max_file_size: 100 });
        assert_eq!(numbers(&h, 7), [3, 4]);
    }
}
