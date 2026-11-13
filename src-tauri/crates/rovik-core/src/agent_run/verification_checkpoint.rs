//! Private restart checkpoints. A command boundary is durable evidence, not a
//! transaction with the shell: an interrupted command always needs approval.

use super::{CheckReceipt, CheckResult, FixRounds, Run};
use crate::agent_run::{change_review::ReviewState, subagent::Ledger, test_writer::TestsState};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

const VERSION: u32 = 1;
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_TRACKED: usize = 10_000;
const MAX_STORE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_STORE_FILES: usize = 32;
static STORE_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) enum Phase {
    AwaitingApproval,
    CommandRunning,
    CheckCompleted,
    FinalizationPending,
}

#[derive(Serialize, Deserialize)]
struct Checkpoint {
    version: u32,
    saved_at: u64,
    dialog_id: String,
    record_id: String,
    root: PathBuf,
    project_root: Option<PathBuf>,
    configuration: String,
    phase: Phase,
    results: Vec<CheckResult>,
    inputs: Option<BTreeMap<String, Option<crate::file_guard::Hash>>>,
    seen: BTreeMap<String, crate::file_guard::Hash>,
    rounds: FixRounds,
    verify_rounds: u32,
    stalled_rounds: u32,
    prepared_again: bool,
    after_partial_rollback: bool,
    integrity_unchecked: bool,
    review: ReviewState,
    tests: TestsState,
    ledger: Ledger,
    /// Absolute task tokens, including already-persisted earlier runs.
    task_spent: u64,
    child_spent: u64,
    receipts: Vec<CheckReceipt>,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

pub(crate) fn directory(data_dir: &Path) -> PathBuf {
    data_dir.join("verification-checkpoints")
}

fn path(dir: &Path, dialog_id: &str) -> PathBuf {
    dir.join(format!(
        "{}.json",
        crate::file_guard::sha256_hex(dialog_id.as_bytes())
    ))
}

pub(crate) fn remove(dir: &Path, dialog_id: &str) {
    if let Err(e) = std::fs::remove_file(path(dir, dialog_id)) {
        if e.kind() != std::io::ErrorKind::NotFound {
            eprintln!("Could not remove verification checkpoint: {e}");
        }
    }
}

#[derive(Debug)]
enum ReadError {
    Expired,
    Unavailable(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Expired => f.write_str("checkpoint expired; checks need fresh results"),
            Self::Unavailable(reason) => f.write_str(reason),
        }
    }
}

impl From<String> for ReadError {
    fn from(reason: String) -> Self {
        Self::Unavailable(reason)
    }
}

impl From<&str> for ReadError {
    fn from(reason: &str) -> Self {
        Self::Unavailable(reason.into())
    }
}

fn read(dir: &Path, dialog_id: &str) -> Result<Option<Checkpoint>, ReadError> {
    let file = match std::fs::File::open(path(dir, dialog_id)) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read checkpoint: {e}").into()),
    };
    if file.metadata().map_err(|e| e.to_string())?.len() > MAX_BYTES as u64 {
        return Err("checkpoint exceeds the private storage limit".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read checkpoint: {e}"))?;
    if bytes.len() > MAX_BYTES {
        return Err("checkpoint exceeds the private storage limit".into());
    }
    let c: Checkpoint = serde_json::from_slice(&bytes)
        .map_err(|e| format!("checkpoint is corrupt or incompatible: {e}"))?;
    if c.version != VERSION || c.dialog_id != dialog_id {
        return Err("checkpoint version or chat identity does not match".into());
    }
    if c.saved_at > now() {
        return Err("checkpoint timestamp is invalid".into());
    }
    if now().saturating_sub(c.saved_at) >= super::CARRY_TTL.as_secs() {
        return Err(ReadError::Expired);
    }
    if c.rounds.baseline.keys().any(|p| !relative(p))
        || c.rounds
            .checkpoint
            .as_ref()
            .is_some_and(|c| c.files.keys().any(|p| !relative(p)))
        || c.seen.keys().any(|p| !relative(p))
        || c.inputs
            .as_ref()
            .is_some_and(|m| m.keys().any(|p| !relative(p)))
    {
        return Err("checkpoint contains an invalid workspace path".into());
    }
    Ok(Some(c))
}

fn relative(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|p| matches!(p, Component::Normal(_) | Component::CurDir))
}

fn identity(path: &str) -> PathBuf {
    crate::file_guard::key(Path::new(path))
}

pub(super) fn configuration(run: &Run) -> String {
    // Check receipts additionally fingerprint toolchain and child environment.
    // This hash binds an approval to the exact commands and permission policy.
    crate::file_guard::sha256_hex(
        json!({"checks": run.checks, "permissions": run.config().permissions,
            "root": run.root.as_deref().map(identity), "projectRoot": run.project_root.as_deref().map(identity)})
            .to_string()
            .as_bytes(),
    )
}

fn inputs(
    root: &Path,
    paths: &[String],
) -> Option<BTreeMap<String, Option<crate::file_guard::Hash>>> {
    if paths.len() > MAX_TRACKED {
        return None;
    }
    let mut bytes_left = MAX_BYTES as u64;
    let mut result = BTreeMap::new();
    for p in paths {
        if !relative(p) {
            return None;
        }
        let file = root.join(p);
        // Do not follow a replaced path outside this workspace, even if a
        // previously known baseline named it.
        if !crate::file_guard::key(&file).starts_with(root) {
            return None;
        }
        let hash = match std::fs::File::open(&file) {
            Ok(f) => {
                let len = f.metadata().ok()?.len();
                if len > bytes_left {
                    return None;
                }
                let mut bytes = Vec::new();
                f.take(bytes_left + 1).read_to_end(&mut bytes).ok()?;
                bytes_left = bytes_left.checked_sub(bytes.len() as u64)?;
                Some(crate::file_guard::hash(&bytes))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return None,
        };
        result.insert(p.clone(), hash);
    }
    Some(result)
}

struct Limited(Vec<u8>);
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_BYTES {
            return Err(std::io::Error::other(
                "checkpoint exceeds the private storage limit",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// Evict expired/oldest private records before an atomic replacement. Limits
// apply across chats, not only to the size of one baseline.
fn make_room(dir: &Path, replacing: &Path, incoming: u64) -> Result<(), String> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .take(1025)
    {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path == replacing || path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let meta = entry.metadata().map_err(|e| e.to_string())?;
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().map_err(|e| e.to_string())?;
        files.push((modified, path, meta.len()));
        if files.len() > 1024 {
            return Err("too many checkpoint files; remove expired checkpoints".into());
        }
    }
    files.sort_by_key(|f| f.0);
    let mut total = files
        .iter()
        .map(|f| f.2)
        .sum::<u64>()
        .saturating_add(incoming);
    let mut count = files.len() + 1;
    for (modified, path, len) in files {
        let expired = modified.elapsed().is_ok_and(|age| age >= super::CARRY_TTL);
        if expired || total > MAX_STORE_BYTES || count > MAX_STORE_FILES {
            std::fs::remove_file(path).map_err(|e| e.to_string())?;
            total = total.saturating_sub(len);
            count = count.saturating_sub(1);
        }
    }
    Ok(())
}

fn write(dir: &Path, checkpoint: &Checkpoint) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    let mut bytes = Limited(Vec::new());
    serde_json::to_writer(&mut bytes, checkpoint).map_err(|e| e.to_string())?;
    let _guard = STORE_WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let path = path(dir, &checkpoint.dialog_id);
    make_room(dir, &path, bytes.0.len() as u64)?;
    crate::atomic_file::write_atomic_private(&path, &bytes.0).map_err(|e| e.to_string())
}

/// Deserialize only statuses the verification gate itself can produce.
pub(super) fn check_status<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<&'static str, D::Error> {
    match String::deserialize(d)?.as_str() {
        "pending" => Ok("pending"),
        "running" => Ok("running"),
        "passed" => Ok("passed"),
        "failed" => Ok("failed"),
        "skipped" => Ok("skipped"),
        "slow" => Ok("slow"),
        "preexisting" => Ok("preexisting"),
        _ => Err(serde::de::Error::custom("unknown check status")),
    }
}

pub(super) fn exit_code<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<i32>>, D::Error> {
    Option::<i32>::deserialize(d).map(Some)
}

impl Run {
    /// Normal completion closes the UI recovery window before releasing the
    /// private checkpoint. A crash never calls this cleanup.
    pub(in crate::agent_run) async fn finish_verification_checkpoint(&mut self) {
        let Some(id) = self.verification.pending_record.take() else {
            return;
        };
        let lookup = id.clone();
        let result = async {
            let rows = self
                .core
                .db
                .read(move |db| crate::chat_db::store::messages_by_id(db, &[lookup]))
                .await?;
            let Some(mut meta) = rows.into_iter().next().and_then(|m| m.meta) else {
                return Ok::<(), String>(());
            };
            meta["verification"]["finalizationPending"] = json!(false);
            self.store_meta(&id, meta).await
        }
        .await;
        if let Err(reason) = result {
            super::log::write(
                &self.core,
                json!({"event":"verificationCheckpointCleanupFailed", "dialogId":self.dialog_id, "reason":reason}),
            );
        }
    }

    pub(super) async fn verification_approval_matches(&mut self, record_id: &str) -> bool {
        let saved = self
            .recent()
            .await
            .ok()
            .and_then(|rows| rows.into_iter().find(|r| r.id == record_id))
            .and_then(|r| r.meta)
            .and_then(|m| {
                m["verification"]["approvalConfiguration"]
                    .as_str()
                    .map(str::to_string)
            });
        if saved.is_some_and(|s| s != configuration(self)) {
            self.verification.approval_changed = true;
            self.verification.checkpoint_notice = Some("Workspace or check configuration changed since approval. Review and approve the current checks again.".into());
            return false;
        }
        true
    }

    pub(super) async fn checkpoint_verification(
        &mut self,
        record_id: &str,
        phase: Phase,
        results: &[CheckResult],
    ) {
        if self.sub().is_some() {
            return;
        }
        let Some(root) = self.root.as_deref().map(identity) else {
            return;
        };
        let dir = directory(&self.core.paths.data_dir);
        let mut paths: Vec<String> = self.verification.carry.baseline.keys().cloned().collect();
        paths.extend(
            self.changed_files
                .iter()
                .map(|p| crate::agent_run::repair::rel_key(root.to_string_lossy().as_ref(), p)),
        );
        paths.sort();
        paths.dedup();
        let seen = paths
            .iter()
            .filter_map(|p| self.seen.get(&root.join(p)).map(|h| (p.clone(), h)))
            .collect();
        let (task_spent, child_spent) = self.core.agents.request_spending(&self.dialog_id);
        let mut checkpoint = Checkpoint {
            version: VERSION,
            saved_at: now(),
            dialog_id: self.dialog_id.clone(),
            record_id: record_id.into(),
            root,
            project_root: self.project_root.as_deref().map(identity),
            configuration: configuration(self),
            phase,
            results: results.to_vec(),
            inputs: None,
            seen,
            rounds: self.verification.carry.clone(),
            verify_rounds: self.verification.verify_rounds,
            stalled_rounds: self.verification.stalled_rounds,
            prepared_again: self.verification.prepared_again,
            after_partial_rollback: self.verification.after_partial_rollback,
            integrity_unchecked: self.verification.integrity_unchecked,
            review: self.review.clone(),
            tests: self.tests.clone(),
            ledger: self.ledger.clone(),
            task_spent,
            child_spent,
            receipts: self.verification.receipts.clone(),
        };
        // Contents stay in the private file, never in chat metadata or logs.
        let result = crate::rt::spawn_blocking(move || {
            checkpoint.inputs = inputs(&checkpoint.root, &paths);
            write(&dir, &checkpoint)
        })
        .await
        .map_err(|e| format!("checkpoint writer stopped: {e}"))
        .and_then(|r| r);
        if let Err(reason) = result {
            // An older checkpoint must not appear to describe this boundary.
            remove(&directory(&self.core.paths.data_dir), &self.dialog_id);
            super::log::write(
                &self.core,
                json!({"event":"verificationCheckpointUnavailable", "dialogId":self.dialog_id, "reason":reason}),
            );
            self.verification.checkpoint_notice =
                Some(format!("Restart checkpoint unavailable: {reason}"));
        }
    }

    /// Restore history and budgets; only a validated opt-in receipt can skip a
    /// command, and the normal integrity/review/acceptance gates still run.
    pub(super) async fn restore_durable_verification(&mut self, record_id: &str) -> bool {
        let dir = directory(&self.core.paths.data_dir);
        let dialog_id = self.dialog_id.clone();
        let loaded = crate::rt::spawn_blocking(move || read(&dir, &dialog_id)).await;
        let c = match loaded {
            Ok(Ok(Some(c))) if c.record_id == record_id => c,
            Ok(Ok(None)) => return false,
            Ok(Err(ReadError::Expired)) => {
                // RunChecks already approved the matching configuration in the
                // database. Age invalidates carried state, not that new click.
                self.verification.checkpoint_notice = Some("Restart checkpoint expired. Checks start fresh and rebuild the baseline where possible.".into());
                remove(&directory(&self.core.paths.data_dir), &self.dialog_id);
                return false;
            }
            Ok(Ok(Some(_))) => {
                self.verification.checkpoint_notice = Some(
                    "Restart checkpoint belongs to another verification round; checks start fresh."
                        .into(),
                );
                return false;
            }
            other => {
                let reason = match other {
                    Ok(Err(e)) => e.to_string(),
                    Err(e) => format!("checkpoint reader stopped: {e}"),
                    _ => unreachable!(),
                };
                self.verification.approval_changed = true;
                self.verification.checkpoint_notice = Some(format!(
                    "Restart checkpoint unavailable: {reason}. Approve fresh checks."
                ));
                remove(&directory(&self.core.paths.data_dir), &self.dialog_id);
                return false;
            }
        };
        let same_root = self.root.as_deref().map(identity).as_ref() == Some(&c.root)
            && self.project_root.as_deref().map(identity) == c.project_root;
        let same_config = configuration(self) == c.configuration;
        if !same_root || !same_config {
            self.verification.approval_changed = true;
            self.verification.checkpoint_notice = Some("Workspace or check configuration changed since approval. Review and approve the current checks again.".into());
        }
        let paths: Vec<String> = c
            .inputs
            .as_ref()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        let root = c.root.clone();
        let current = if same_root && same_config {
            crate::rt::spawn_blocking(move || inputs(&root, &paths))
                .await
                .ok()
                .flatten()
        } else {
            None
        };
        let unchanged = same_root && same_config && c.inputs.is_some() && current == c.inputs;
        let mut rounds = c.rounds;
        if !same_root {
            rounds.baseline.clear();
            rounds.groups.clear();
            rounds.worker_steps.clear();
            rounds.last_triage = None;
        }
        self.verification.restore(rounds);
        self.verification.verify_rounds = self.verification.verify_rounds.max(c.verify_rounds);
        self.verification.stalled_rounds = c.stalled_rounds;
        self.verification.prepared_again = c.prepared_again;
        self.verification.after_partial_rollback = c.after_partial_rollback;
        self.verification.integrity_unchecked = c.integrity_unchecked || !unchanged;
        self.verification.writes_since_verify = true;
        if c.phase == Phase::FinalizationPending {
            self.verification.pending_record = Some(record_id.to_string());
        }
        self.review = c.review;
        self.tests = c.tests;
        self.ledger = c.ledger;
        self.core
            .agents
            .restore_request_spending(&self.dialog_id, c.task_spent, c.child_spent);
        if unchanged {
            for (p, hash) in c.seen {
                self.seen.record_hash(&c.root.join(p), hash);
            }
            // Each receipt is revalidated against the complete current input
            // stamp in run_checks, including check config and environment.
            self.verification.receipts = c
                .receipts
                .into_iter()
                .filter(|r| r.check.reuse_success && r.result.status == "passed")
                .collect();
        } else {
            self.verification.carry.checkpoint = None;
            self.verification.receipts.clear();
        }
        if same_root && same_config {
            self.verification.checkpoint_notice = Some(if unchanged {
            "Restored verification history after restart. Unconfirmed commands require approval; completed default checks run again."
        } else {
            "Workspace inputs changed or could not be checked after restart. Repair history was retained; rollback and result reuse are disabled, and integrity is incomplete."
        }.into());
        }
        true
    }
}

/// The daemon recovers only the exact current verification record. It never
/// resumes a shell command or adopts an earlier record after a newer turn.
pub(crate) async fn after_crash(
    core: &crate::core::Core,
    dialog_id: &str,
    last: &crate::chat_db::store::MessageRow,
) -> Result<bool, String> {
    let Some(v) = last.meta.as_ref().and_then(|m| m.get("verification")) else {
        return Ok(false);
    };
    if !matches!(
        v["status"].as_str(),
        Some("running" | "needs-approval" | "passed" | "failed")
    ) {
        return Ok(false);
    }
    if matches!(v["status"].as_str(), Some("passed" | "failed")) && v["finalizationPending"] != true
    {
        return Ok(false);
    }
    let dir = directory(&core.paths.data_dir);
    let id = dialog_id.to_string();
    let loaded = crate::rt::spawn_blocking(move || read(&dir, &id))
        .await
        .map_err(|e| e.to_string())?;
    let reason = match loaded {
        Ok(Some(c)) if c.record_id == last.id => match c.phase {
            Phase::CommandRunning => "A verification command was interrupted; its outcome is unknown. Approve checks to run again before trusting the result.",
            _ => "Verification paused after restart. Approve checks to resume from confirmed results; default checks run again.",
        }.to_string(),
        Ok(_) if v["status"] == "running" || v["finalizationPending"] == true => "Verification was interrupted without a matching checkpoint. Approve fresh checks; repair state and integrity may be incomplete.".into(),
        Ok(_) => return Ok(false),
        Err(e) => format!("Verification checkpoint unavailable: {e}. Approve fresh checks; integrity may be incomplete."),
    };
    let mut meta = last.meta.clone().unwrap_or_else(|| json!({}));
    meta["verification"]["status"] = json!("needs-approval");
    meta["verification"]["reason"] = json!(reason);
    meta["verification"]["checkpointRecovery"] = json!("approval-required");
    let id = last.id.clone();
    let patch = crate::chat_db::store::MessagePatch {
        meta: Some(meta.clone()),
        ..Default::default()
    };
    core.db
        .write(move |db| crate::chat_db::store::update_message(db, &id, &patch))
        .await?;
    core.emit(crate::agent_run::events::AgentEvent::MessagePatched {
        dialog_id: dialog_id.into(),
        message_id: last.id.clone(),
        patch: crate::agent_run::events::Patch {
            meta: Some(meta),
            ..Default::default()
        },
    });
    Ok(true)
}

/// Test the same elapsed time for in-memory and persisted copies.
#[cfg(test)]
pub(in crate::agent_run) fn age_for_test(dir: &Path, dialog_id: &str, by: std::time::Duration) {
    let Some(mut checkpoint) = read(dir, dialog_id).unwrap() else {
        return;
    };
    checkpoint.saved_at = checkpoint.saved_at.saturating_sub(by.as_secs());
    write(dir, &checkpoint).unwrap();
}

#[cfg(test)]
static INTERRUPT_AT: std::sync::Mutex<std::collections::BTreeMap<String, Phase>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

#[cfg(test)]
pub(super) fn interrupt_at(dialog_id: &str, phase: Phase) -> Result<(), String> {
    let mut cuts = INTERRUPT_AT.lock().unwrap_or_else(|e| e.into_inner());
    if cuts.get(dialog_id) == Some(&phase) {
        cuts.remove(dialog_id);
        return Err("injected process interruption at verification boundary".into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "verification_checkpoint_tests.rs"]
mod tests;
