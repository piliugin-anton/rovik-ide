//! Thorough's two implementers (docs/design/autopilot-plan.md 4.5, 4.9,
//! D14; T05, T06): a Thorough task's plan is implemented twice, each time
//! from the same start, in a working copy of its own, and the better of
//! the two is the task's result.
//!
//! The copies (`Copies`): locally, a child overlay of the task chat's own
//! overlay each (its changes so far, the tests written first among them,
//! under it); on a server, a worktree beside the chat's, which the daemon
//! makes (`AgentState::set_copies`). A chat with neither (one that works
//! in the project folder) has one implementer, as in Balanced, and its
//! Pick record says why.
//!
//! The candidates, "a" then "b", each a chat of its own under the task's
//! (`create_child_dialog`, kind "candidate", so no list shows it) whose
//! run is an "Implement this plan" run of the task's, at its effort and
//! with the task chat's configuration: its change review, its checks, its
//! adversarial tester and its acceptance check run in it, as in any
//! implementation's run, and its stage records are its own chat's, each
//! with its letter (`Stage::in_candidates`). They implement one
//! after the other, in the task chat's Implement stage, whose run is the
//! IDE's (`candidates_stage`): it starts each and waits for its records to
//! end, so the chat is running, and its Stop stops them. A restart picks
//! the one under way up where it was.
//!
//! The task chat's Review, Checks and Break records say how each went; its
//! Pick (4.9, D14) scores them by their records and copies: the tests
//! written first that pass in the copy, the checks that passed, the high
//! and medium findings left open, the lines changed, and the judge
//! (judge.rs) decides only a tie. The winner's change becomes the chat's
//! (its overlay, or its worktree) and lands from there as an
//! implementation's would; the loser's diff stays in the Pick's record.
//! The Acceptance record, the report and the reviewer's view are the
//! winner's.

use super::autopilot::{
    self, add_stage, end_stage, load, next_stage, records_of, Next, Recorded, Settings, Stage,
    StageRecord, Status,
};
use super::budget::{self, Spent};
use super::change_review::{render_diff, FileChange};
use super::events::AgentEvent;
use super::judge;
use super::run::{new_message, Run};
use super::subagent::{self, Kind, SubOutcome, SubagentSettings};
use super::StartKind;
use crate::chat_db::store::{self, char_prefix, NewChildDialog};
use crate::core::Core;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cmp::Reverse;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The candidates, in the order they implement.
pub const LETTERS: [&str; 2] = ["a", "b"];

/// A candidate's chat's kind (`dialogs.kind`).
pub const KIND: &str = "candidate";

/// How the task chat's Implement run ends once its candidates did: the
/// pick takes the outcome over (it's the winner's).
pub const CANDIDATES_OUTCOME: &str = "candidates";

/// How often the task's chat looks at where its candidate is.
const POLL: Duration = Duration::from_millis(100);
/// How often, while the candidate's stage waits out the provider's rate
/// limit (H07), which can take up to an hour.
const WAIT_POLL: Duration = Duration::from_secs(1);

/// What the Pick's record keeps of the losing candidate's diff.
pub const MAX_KEPT_DIFF_CHARS: usize = 60_000;

/// What the judge gets of each candidate's diff.
const JUDGE_DIFF_CHARS: usize = 30_000;

/*
 * ---------------------------------------------------------------------------
 * The copies
 * ---------------------------------------------------------------------------
 */

pub type Copying<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Where a task's candidates implement (T05): a working copy each of the
/// task chat's folder as its changes so far have it, and what becomes of
/// it. Locally the chat's overlay makes them (`OverlayCopies`); a server's
/// daemon sets its own (`AgentState::set_copies`).
pub trait Copies: Send + Sync {
    /// `chat`'s copy of `main`'s folder (`project`, `main`'s project folder,
    /// where it's needed): made, or the one made before. Its folder, where
    /// `chat`'s runs work.
    fn open(
        &self,
        main: String,
        chat: String,
        project: Option<String>,
    ) -> Copying<Result<String, String>>;
    /// What `chat` changed in its copy: each file as `main`'s folder has
    /// it and as the copy has it.
    fn change(&self, main: String, chat: String) -> Copying<Result<Vec<FileChange>, String>>;
    /// `chat`'s change becomes `main`'s: written into `main`'s folder, and
    /// the copy gone. The files it wrote.
    fn adopt(&self, main: String, chat: String) -> Copying<Result<Vec<String>, String>>;
    /// `chat`'s copy, gone.
    fn discard(&self, main: String, chat: String) -> Copying<()>;
    /// Whether `chat`'s copy is still there: made, and neither taken nor
    /// thrown away (what a pick a crash cut asks, R03).
    fn kept(&self, main: String, chat: String) -> Copying<bool>;
}

/// A local chat's copies: a child overlay of the chat's overlay each
/// (F02), landed into it when it wins.
pub struct OverlayCopies;

impl Copies for OverlayCopies {
    fn open(
        &self,
        main: String,
        chat: String,
        project: Option<String>,
    ) -> Copying<Result<String, String>> {
        Box::pin(async move {
            let opened = crate::rt::spawn_blocking(move || {
                use crate::overlay::chats;
                if chats::get(&main).is_none() {
                    let project = project.ok_or("the chat has no project folder")?;
                    chats::open(&main, Path::new(&project), Box::new(|_, _| {}))?;
                }
                // A restart forgot them: the candidates share the room again.
                let overlay = match chats::reopen_child(&main, &chat, LETTERS.len()) {
                    Some(o) => o,
                    None => chats::open_child_shared(&main, &chat, LETTERS.len())?,
                };
                Ok::<_, String>(overlay.view.to_string_lossy().into_owned())
            });
            opened.await.map_err(|e| e.to_string())?
        })
    }

    fn change(&self, _main: String, chat: String) -> Copying<Result<Vec<FileChange>, String>> {
        Box::pin(async move {
            let read = crate::rt::spawn_blocking(move || {
                use crate::overlay::chats;
                let overlay = chats::get(&chat).ok_or("the candidate's copy isn't open")?;
                overlay.fs.flush();
                let text = |root: &Path, path: &str| {
                    std::fs::read(root.join(path))
                        .ok()
                        .map(|b| String::from_utf8_lossy(&b).into_owned())
                };
                let mut files = Vec::new();
                for change in chats::changes(&chat) {
                    let path = change.path().replace('\\', "/");
                    // Under it, the chat's own view: what it changes there.
                    let (before, after) =
                        (text(&overlay.project, &path), text(&overlay.view, &path));
                    if before != after {
                        files.push(FileChange {
                            path,
                            before,
                            after,
                        });
                    }
                }
                files.sort_by(|a, b| a.path.cmp(&b.path));
                Ok::<_, String>(files)
            });
            read.await.map_err(|e| e.to_string())?
        })
    }

    fn adopt(&self, _main: String, chat: String) -> Copying<Result<Vec<String>, String>> {
        Box::pin(async move {
            let landed = crate::rt::spawn_blocking(move || {
                use crate::overlay::chats;
                let landed = chats::land_child_except(&chat, &[]).map_err(|e| e.to_string())?;
                chats::discard_child(&chat);
                Ok::<_, String>(landed)
            });
            landed.await.map_err(|e| e.to_string())?
        })
    }

    fn discard(&self, _main: String, chat: String) -> Copying<()> {
        Box::pin(async move {
            let _ = crate::rt::spawn_blocking(move || crate::overlay::chats::discard_child(&chat))
                .await;
        })
    }

    fn kept(&self, main: String, chat: String) -> Copying<bool> {
        Box::pin(async move {
            let kept =
                crate::rt::spawn_blocking(move || crate::overlay::chats::child_kept(&main, &chat));
            kept.await.unwrap_or(false)
        })
    }
}

/// The copies a task's candidates get here, or why there are none: a
/// server's daemon's; locally, an isolated chat's overlay's.
fn copies_for(core: &Core, settings: &Settings) -> Result<Arc<dyn Copies>, String> {
    if let Some(copies) = core.agents.copies() {
        return Ok(copies);
    }
    if core.agents.on_server() {
        return Err("this server makes no second working copy of the chat's folder".into());
    }
    let isolated = settings
        .isolate
        .unwrap_or_else(crate::overlay::chats::enabled);
    if !isolated || settings.root.is_none() {
        return Err(
            "the chat works in the project folder itself, so there's no copy of its work to give a second implementer"
                .into(),
        );
    }
    Ok(Arc::new(OverlayCopies))
}

/// The copies of a task whose candidates ran here.
fn copies_here(core: &Core) -> Arc<dyn Copies> {
    core.agents
        .copies()
        .unwrap_or_else(|| Arc::new(OverlayCopies))
}

/*
 * ---------------------------------------------------------------------------
 * The records
 * ---------------------------------------------------------------------------
 */

/// The candidates of a task's implementation, in the task's chat
/// (`meta.candidates`): each candidate's chat and how it went. Also, with
/// one implementer, why there was one (`status` "single").
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CandidatesRecord {
    pub task: String,
    /// The implementation's round it's for.
    #[serde(default = "first_round")]
    pub round: u32,
    /// running, done, stopped; single: one implementer, for `reason`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<CandidateState>,
}

fn first_round() -> u32 {
    1
}

/// A candidate, as the task's chat records it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CandidateState {
    pub letter: String,
    /// Its chat.
    pub chat: String,
    /// waiting, running, done, failed, skipped, stopped.
    pub status: String,
    /// How its implementation's run ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// How it went, in a line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// D19: the installs its copy got before it started, in a line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared: Option<String>,
}

/// The task's candidates' records in the chat, oldest first, with their
/// rows' ids.
async fn rows_of(
    core: &Core,
    dialog_id: &str,
    task: &str,
) -> Result<Vec<(String, CandidatesRecord)>, String> {
    let all = all_rows(core, dialog_id).await?;
    Ok(all.into_iter().filter(|(_, r)| r.task == task).collect())
}

/// Every round's record of candidates in the chat, of any of its tasks.
async fn all_rows(core: &Core, dialog_id: &str) -> Result<Vec<(String, CandidatesRecord)>, String> {
    let id = dialog_id.to_string();
    let rows = core
        .db
        .read(move |c| store::messages_with_meta(c, &id, "candidates", None))
        .await?;
    Ok(rows
        .iter()
        .filter_map(|m| {
            let record: CandidatesRecord =
                serde_json::from_value(m.meta.as_ref()?.get("candidates")?.clone()).ok()?;
            Some((m.id.clone(), record))
        })
        .collect())
}

/// A round's candidates, done with: their copies gone, and their runs'
/// registrations.
async fn discard_round(
    core: &Core,
    dialog_id: &str,
    copies: &dyn Copies,
    round: &CandidatesRecord,
) {
    for c in &round.candidates {
        copies.discard(dialog_id.to_string(), c.chat.clone()).await;
        core.agents.drop_candidate(&c.chat);
    }
}

/// The candidates' record in row `id`.
async fn row_by_id(
    core: &Core,
    dialog_id: &str,
    id: &str,
) -> Result<Option<CandidatesRecord>, String> {
    record_in_row(core, dialog_id, id, "candidates").await
}

/// The IDE's record in row `id` of the chat (`meta.<key>`), if it holds one
/// that reads.
async fn record_in_row<T: serde::de::DeserializeOwned>(
    core: &Core,
    dialog_id: &str,
    id: &str,
    key: &'static str,
) -> Result<Option<T>, String> {
    let ids = vec![id.to_string()];
    let rows = core
        .db
        .read(move |c| store::messages_by_id(c, &ids))
        .await?;
    Ok(rows
        .into_iter()
        .find(|m| m.dialog_id == dialog_id)
        .and_then(|m| serde_json::from_value(m.meta?.get(key)?.clone()).ok()))
}

/// The record in row `id` changed to `value` (`meta.<key>`), and shown.
async fn save_row(
    core: &Core,
    dialog_id: &str,
    id: &str,
    key: &str,
    value: Value,
) -> Result<(), String> {
    autopilot::save_meta(core, dialog_id, id, json!({ key: value })).await
}

/// T05: the chat whose task `chat` implements, when it's a candidate's.
pub async fn main_of(core: &Core, chat: &str) -> Result<Option<String>, String> {
    let id = chat.to_string();
    let child = core.db.read(move |c| store::child_dialog(c, &id)).await?;
    Ok(child
        .filter(|(d, _)| d.kind == KIND)
        .map(|(d, _)| d.parent_id))
}

/// Why a chat doesn't move while its task's candidates are at work, or
/// wait for their Pick (R03, as decided).
pub const MOVE_AWAITS_PICK: &str = "This chat's task is being implemented by two candidates, whose working copies can't move with it: move it once the Pick has chosen between them.";

/// Whether the chat's task has candidates at work, or waiting for their
/// Pick: from their implementation, under way or done, until the Pick
/// ends. Their working copies (a local overlay's, a server's worktrees)
/// don't move with the chat, so a move waits until then (R03).
pub async fn awaiting_pick(core: &Core, dialog_id: &str) -> Result<bool, String> {
    let Some((_, records)) = autopilot::current_task(core, dialog_id).await? else {
        return Ok(false);
    };
    if autopilot::next_after(&records) == Next::Over {
        return Ok(false);
    }
    let by_candidates = |r: &&Recorded| {
        r.record.name == Stage::Implement
            && !r.record.candidates.is_empty()
            && r.record.status != Status::Failed
    };
    let Some(at) = records.iter().rposition(|r| by_candidates(&r)) else {
        return Ok(false);
    };
    let picked = records[at + 1..]
        .iter()
        .any(|r| r.record.name == Stage::Pick && r.record.status != Status::Running);
    Ok(!picked)
}

/// Whether the task's latest implementation was its candidates'.
pub fn ran(records: &[Recorded]) -> bool {
    records
        .iter()
        .rev()
        .find(|r| r.record.name == Stage::Implement)
        .is_some_and(|r| !r.record.candidates.is_empty())
}

/// A chat's records of `task`, oldest first.
async fn task_records(core: &Core, chat: &str, task: &str) -> Result<Vec<Recorded>, String> {
    let all = load(core, chat).await?;
    Ok(records_of(&all, task).into_iter().cloned().collect())
}

/// What the candidates that are over spent, by their chats' records
/// (U04): the task chat's Implement record keeps it, so the task's
/// records add up to what it spent (`autopilot::task_spent`).
async fn spent_by(core: &Core, row: &CandidatesRecord) -> Result<Spent, String> {
    let mut total = Spent::default();
    for c in row
        .candidates
        .iter()
        .filter(|c| !matches!(c.status.as_str(), "waiting" | "running"))
    {
        let theirs = task_records(core, &c.chat, &row.task).await?;
        total = autopilot::add_spent(total, theirs.iter().map(|r| &r.record));
    }
    Ok(total)
}

/// The Pick's line for a task implemented once: why it was.
pub async fn single_summary(core: &Core, dialog_id: &str, task: &str) -> Result<String, String> {
    let why = rows_of(core, dialog_id, task)
        .await?
        .into_iter()
        .rev()
        .find(|(_, r)| r.status == "single")
        .and_then(|(_, r)| r.reason);
    Ok(match why {
        Some(why) => format!("One implementer: {why}"),
        None => "One implementer".into(),
    })
}

/// The tool call id a candidate's events and transcript go by.
fn call_id(row: &str, letter: &str) -> String {
    format!("{row}:{letter}")
}

/// A stage record's status in a word, for a line without a summary.
fn status_word(status: Status) -> &'static str {
    match status {
        Status::Running => "under way",
        Status::Done => "done",
        Status::Failed => "failed",
        Status::Skipped => "skipped",
    }
}

/*
 * ---------------------------------------------------------------------------
 * The Implement stage
 * ---------------------------------------------------------------------------
 */

/// T05: a Thorough task's implementation, by candidates: their copies,
/// their chats, the task chat's record of them, then the IDE's run that
/// carries them out (`candidates_stage`). Some(true): it started; Some(false):
/// it couldn't, and its record says why. None: no copies could be made,
/// and a record says why; the task is implemented once (the caller's).
pub(super) async fn start(
    core: &Core,
    dialog_id: &str,
    records: &[Recorded],
    round: u32,
    settings: &Settings,
) -> Result<Option<bool>, String> {
    let first = &records[0].record;
    let (task, effort) = (first.task.clone(), first.effort);
    let made = make(
        core,
        dialog_id,
        records,
        round,
        settings.root.clone(),
        || copies_for(core, settings),
    )
    .await?;
    let row = match made {
        Ok(row) => row,
        Err(why) => {
            let single = CandidatesRecord {
                task: task.clone(),
                round,
                status: "single".into(),
                reason: Some(why.clone()),
                candidates: Vec::new(),
            };
            autopilot::add_record_row(core, dialog_id, "candidates", json!(single)).await?;
            super::log::write(
                core,
                json!({ "event": "candidates", "dialogId": dialog_id, "status": "single", "reason": why }),
            );
            return Ok(None);
        }
    };
    let record = StageRecord {
        candidates: LETTERS.iter().map(|l| l.to_string()).collect(),
        reference: Some(row.clone()),
        summary: Some("Two implementers, one after the other".into()),
        ..StageRecord::running(&task, Stage::Implement, round, effort)
    };
    let id = add_stage(core, dialog_id, &record).await?;
    // The IDE's run takes the stage up, as a Retry does.
    let started = super::start_run(core, settings.request(dialog_id, StartKind::Retry)).await;
    let Some(why) = autopilot::not_started_why(started) else {
        return Ok(Some(true));
    };
    // Never started, its round is never picked (R03).
    if let Some(round) = row_by_id(core, dialog_id, &row).await? {
        discard_round(core, dialog_id, copies_here(core).as_ref(), &round).await;
    }
    end_stage(core, dialog_id, &id, |r| {
        r.status = Status::Failed;
        r.outcome = Some("error".into());
        r.summary = Some(why);
    })
    .await?;
    Ok(Some(false))
}

/// The candidates of an implementation's round: their copies first (Err:
/// why none can be made), then their chats, and the task chat's record of
/// them, whose row it returns.
async fn make(
    core: &Core,
    dialog_id: &str,
    records: &[Recorded],
    round: u32,
    project: Option<String>,
    copies: impl FnOnce() -> Result<Arc<dyn Copies>, String>,
) -> Result<Result<String, String>, String> {
    let task = records[0].record.task.clone();
    let copies = match copies() {
        Ok(copies) => copies,
        Err(why) => return Ok(Err(why)),
    };
    // Earlier rounds' copies go first, this task's or an earlier one's: none
    // of them will be picked now, and kept, they'd take the room the new
    // copies share (R03).
    for (_, earlier) in all_rows(core, dialog_id).await? {
        discard_round(core, dialog_id, copies.as_ref(), &earlier).await;
    }
    let chats: Vec<String> = LETTERS
        .iter()
        .map(|_| uuid::Uuid::new_v4().to_string())
        .collect();
    for (letter, chat) in LETTERS.iter().zip(&chats) {
        let opened = copies
            .open(dialog_id.to_string(), chat.clone(), project.clone())
            .await;
        if let Err(why) = opened {
            for chat in &chats {
                copies.discard(dialog_id.to_string(), chat.clone()).await;
            }
            return Ok(Err(format!(
                "candidate {letter}'s working copy couldn't be made: {why}"
            )));
        }
    }
    let title = match autopilot::task_plan_file(core, dialog_id).await? {
        Some(file) => std::fs::read_to_string(file)
            .ok()
            .and_then(|plan| autopilot::plan_title(&plan)),
        None => None,
    };
    let row = new_message("assistant", "");
    for (letter, chat) in LETTERS.iter().zip(&chats) {
        let dialog = NewChildDialog {
            id: chat.clone(),
            parent_id: dialog_id.to_string(),
            parent_call_id: call_id(&row.id, letter),
            kind: KIND.into(),
            title: match &title {
                Some(title) => format!("Candidate {letter}: {title}"),
                None => format!("Candidate {letter}"),
            },
            created_at: store::now_ms(),
        };
        core.db
            .write(move |c| store::create_child_dialog(c, &dialog))
            .await?;
    }
    let record = CandidatesRecord {
        task,
        round,
        status: "running".into(),
        reason: None,
        candidates: LETTERS
            .iter()
            .zip(&chats)
            .map(|(letter, chat)| CandidateState {
                letter: letter.to_string(),
                chat: chat.clone(),
                status: "waiting".into(),
                outcome: None,
                summary: None,
                prepared: None,
            })
            .collect(),
    };
    let mut row = row;
    row.meta = Some(json!({ "candidates": record }));
    let id = row.id.clone();
    autopilot::append(core, dialog_id, row).await?;
    super::log::write(
        core,
        json!({ "event": "candidates", "dialogId": dialog_id, "status": "running", "chats": chats }),
    );
    Ok(Ok(id))
}

/// How a candidate went, by its records once they're over: its status in
/// the task's record, its implementation's outcome, and a line.
async fn how_it_went(
    core: &Core,
    chat: &str,
    task: &str,
) -> Result<(String, Option<String>, String), String> {
    let mine = task_records(core, chat, task).await?;
    let Some(implement) = mine.iter().find(|r| r.record.name == Stage::Implement) else {
        return Ok(("failed".into(), None, "It never started".into()));
    };
    let outcome = implement.record.outcome.clone();
    let failed = mine.iter().find(|r| r.record.status == Status::Failed);
    let (status, summary) = match failed {
        Some(r) if r.record.outcome.as_deref() == Some("stopped") => {
            ("stopped".to_string(), "Stopped".to_string())
        }
        Some(r) => (
            "failed".to_string(),
            r.record
                .summary
                .clone()
                .unwrap_or_else(|| status_word(r.record.status).into()),
        ),
        None => {
            let checks = mine.iter().rev().find(|r| r.record.name == Stage::Checks);
            let line = checks
                .and_then(|r| r.record.summary.clone())
                .or_else(|| implement.record.summary.clone())
                .unwrap_or_else(|| "Finished".into());
            ("done".to_string(), line)
        }
    };
    Ok((status, outcome, summary))
}

/// A candidate's implementation, recorded in its chat and started there,
/// in its copy (`settings.root`). False: it couldn't start, and its record
/// says why.
async fn start_candidate(
    core: &Core,
    main: &str,
    records: &[Recorded],
    state: &CandidateState,
    settings: &Settings,
    project: Option<&str>,
) -> Result<bool, String> {
    let first = &records[0].record;
    let record = StageRecord {
        candidate: Some(state.letter.clone()),
        ..StageRecord::running(&first.task, Stage::Implement, 1, first.effort)
    };
    let id = add_stage(core, &state.chat, &record).await?;
    #[cfg(test)]
    if autopilot::crashes(main, autopilot::Crash::BeforeCandidate) {
        return Ok(true);
    }
    let built =
        autopilot::implementation_message(core, main, records, &id, project, Some(&state.letter))
            .await?;
    let why = match built {
        Ok(message) => {
            let request = settings.request(&state.chat, StartKind::Send { message });
            match autopilot::not_started_why(super::start_run_boxed(core, request).await) {
                None => return Ok(true),
                Some(why) => why,
            }
        }
        Err(why) => why,
    };
    end_stage(core, &state.chat, &id, |r| {
        r.status = Status::Failed;
        r.outcome = Some("error".into());
        r.summary = Some(why);
    })
    .await?;
    Ok(false)
}

/// A candidate's records that can't go on: the one under way ends, failed
/// with `why`, and so do they.
async fn give_up(
    core: &Core,
    chat: &str,
    mine: &[Recorded],
    outcome: &str,
    why: &str,
) -> Result<(), String> {
    match mine
        .iter()
        .rev()
        .find(|r| r.record.status == Status::Running)
    {
        Some(r) => {
            end_stage(core, chat, &r.id, |r| {
                r.status = Status::Failed;
                r.outcome = Some(outcome.to_string());
                r.summary = Some(why.to_string());
            })
            .await?;
        }
        None => {
            let last = &mine[mine.len() - 1].record;
            let mut record = StageRecord::ended(
                &last.task,
                last.name,
                last.round,
                last.effort,
                Status::Failed,
            )
            .with_summary(why);
            record.outcome = Some(outcome.to_string());
            record.candidate = last.candidate.clone();
            add_stage(core, chat, &record).await?;
        }
    }
    Ok(())
}

impl Run {
    /// T05: the Implement stage of a task done by candidates: each in turn
    /// implements the plan in its copy and its own chat, in a run of its
    /// own, and this run waits for its records to end. "stopped" when the
    /// user stopped the task; else the stage's record says how each went,
    /// and the pick decides the rest (`CANDIDATES_OUTCOME`).
    pub(super) async fn candidates_stage(&mut self) -> Result<String, String> {
        let Some(stage) = self.stage.clone() else {
            return Ok("done".into());
        };
        let (core, main) = (self.core.clone(), self.dialog_id.clone());
        let Some((task, records)) = autopilot::current_task(&core, &main).await? else {
            return Ok("done".into());
        };
        let Some(record) = records.iter().find(|r| r.id == stage.record).cloned() else {
            return Err(format!("its stage record {} is gone", stage.record));
        };
        // A Retry's round has its candidates made now.
        let found = match record.record.reference.clone() {
            Some(id) => row_by_id(&core, &main, &id).await?.map(|row| (id, row)),
            None => None,
        };
        let (row_id, mut row) = match found {
            Some(found) => found,
            None => {
                let settings = self.main_settings();
                let made = make(
                    &core,
                    &main,
                    &records,
                    record.record.round,
                    self.project_root.clone(),
                    || copies_for(&core, &settings),
                )
                .await?;
                let id = match made {
                    Ok(id) => id,
                    Err(why) => {
                        end_stage(&core, &main, &stage.record, |r| {
                            r.status = Status::Failed;
                            r.outcome = Some("error".into());
                            r.summary = Some(format!("No candidates: {why}"));
                        })
                        .await?;
                        return Ok("done".into());
                    }
                };
                end_stage(&core, &main, &stage.record, |r| {
                    r.reference = Some(id.clone());
                })
                .await?;
                let row = row_by_id(&core, &main, &id)
                    .await?
                    .ok_or("its candidates' record is gone")?;
                (id, row)
            }
        };
        let copies = copies_here(&core);
        for i in 0..row.candidates.len() {
            if self.stopped() {
                break;
            }
            // A move or the server's stop pauses the task here (6.8, R02).
            if let Some(pause) = self.pause.get() {
                return self.pause_for(pause).await;
            }
            let state = row.candidates[i].clone();
            if !matches!(state.status.as_str(), "waiting" | "running") {
                continue;
            }
            // Past the task's budget, a candidate still to start doesn't
            // (U04): the task goes on with what the first did.
            if state.status == "waiting" && i > 0 {
                let spent = autopilot::task_spent(&core, &main).await?;
                let budget = self.config().autopilot.budget;
                if let Some(why) = budget::over(&budget, &spent) {
                    row.candidates[i].status = "skipped".into();
                    row.candidates[i].summary =
                        Some(format!("Not started: over the task's budget: {why}"));
                    save_row(&core, &main, &row_id, "candidates", json!(row)).await?;
                    continue;
                }
            }
            let opened = copies
                .open(main.clone(), state.chat.clone(), self.project_root.clone())
                .await;
            let folder = match opened {
                Ok(folder) => folder,
                Err(why) => {
                    row.candidates[i].status = "failed".into();
                    row.candidates[i].summary =
                        Some(format!("Its working copy couldn't be opened: {why}"));
                    save_row(&core, &main, &row_id, "candidates", json!(row)).await?;
                    continue;
                }
            };
            let call = call_id(&row_id, &state.letter);
            core.agents
                .add_candidate(&state.chat, &main, self.project_root.clone(), &call);
            core.emit(AgentEvent::SubagentStarted {
                dialog_id: main.clone(),
                tool_call_id: call.clone(),
                child_dialog_id: state.chat.clone(),
                kind: KIND.into(),
                model: self.profile.label.clone(),
                goal: format!("Candidate {}", state.letter),
            });
            row.candidates[i].status = "running".into();
            save_row(&core, &main, &row_id, "candidates", json!(row)).await?;
            // D19: its copy's dependencies before it starts: a server's
            // worktree has none of the chat's installs, which git ignores.
            if task_records(&core, &state.chat, &task).await?.is_empty() {
                if let Some(line) = self.prepare_copy(&state.chat, &folder).await {
                    row.candidates[i].prepared = Some(line);
                    save_row(&core, &main, &row_id, "candidates", json!(row)).await?;
                }
            }
            let settings = Settings {
                root: Some(folder),
                isolate: Some(false),
                ..self.main_settings()
            };
            if let Driven::Paused(pause) = self.drive_candidate(&records, &state, &settings).await?
            {
                return self.pause_for(pause).await;
            }
            let (status, outcome, summary) = how_it_went(&core, &state.chat, &task).await?;
            row.candidates[i].status = status.clone();
            row.candidates[i].outcome = outcome;
            row.candidates[i].summary = Some(summary);
            save_row(&core, &main, &row_id, "candidates", json!(row)).await?;
            // What it spent counts for the task from here (U04).
            let spent = spent_by(&core, &row).await?;
            end_stage(&core, &main, &stage.record, |r| r.spent = Some(spent)).await?;
            core.emit(AgentEvent::SubagentEnded {
                dialog_id: main.clone(),
                tool_call_id: call,
                child_dialog_id: state.chat.clone(),
                status,
            });
        }
        let stopped = self.stopped();
        row.status = if stopped { "stopped" } else { "done" }.into();
        save_row(&core, &main, &row_id, "candidates", json!(row)).await?;
        // What they spent, all of it: a crash between a candidate's end and
        // the record of its spend loses none (R03).
        let spent = spent_by(&core, &row).await?;
        end_stage(&core, &main, &stage.record, |r| r.spent = Some(spent)).await?;
        super::log::write(
            &core,
            json!({
                "event": "candidates",
                "dialogId": main,
                "status": row.status,
                "candidates": row.candidates.iter().map(|c| json!({ "letter": c.letter, "status": c.status, "outcome": c.outcome })).collect::<Vec<_>>(),
            }),
        );
        if stopped {
            // A round a Stop ended is never picked: a Retry makes another.
            discard_round(&core, &main, copies.as_ref(), &row).await;
            return Ok("stopped".into());
        }
        let lines = row
            .candidates
            .iter()
            .map(|c| {
                format!(
                    "{}: {}",
                    c.letter,
                    c.summary.as_deref().unwrap_or(&c.status)
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        end_stage(&core, &main, &stage.record, |r| {
            r.status = Status::Done;
            r.outcome = Some(CANDIDATES_OUTCOME.into());
            r.summary = Some(lines);
        })
        .await?;
        Ok(CANDIDATES_OUTCOME.into())
    }

    /// What this run started with, for the task chat's later runs and its
    /// candidates' (their folders their own).
    fn main_settings(&self) -> Settings {
        Settings {
            profile: self.profile.clone(),
            options: self.options.clone(),
            context_mode: Some(self.context_mode.clone()),
            root: self.project_root.clone(),
            local_date: Some(self.local_date.clone()),
            isolate: Some(crate::overlay::chats::get(&self.dialog_id).is_some()),
        }
    }

    /// Runs the candidate until its records are over: started, or picked
    /// up where a restart left it (twice at most, then it ends failed).
    /// A pause asked of this run (a move, the server's stop) is the
    /// candidate's first, at its next safe point; the task's next start
    /// goes on with both.
    async fn drive_candidate(
        &mut self,
        records: &[Recorded],
        state: &CandidateState,
        settings: &Settings,
    ) -> Result<Driven, String> {
        let core = self.core.clone();
        let (chat, task) = (state.chat.as_str(), records[0].record.task.clone());
        let effort = records[0].record.effort;
        let mut picked_up = 0;
        loop {
            if let Some(pause) = self.pause.get() {
                if !core.agents.is_running(chat) {
                    return Ok(Driven::Paused(pause));
                }
                core.agents.pause(chat, pause);
            }
            if core.agents.is_running(chat) {
                tokio::time::sleep(POLL).await;
                continue;
            }
            // Its driver's turn: once it's free, its records say where it is.
            let Some(driving) = autopilot::try_drive_lock(chat) else {
                tokio::time::sleep(POLL).await;
                continue;
            };
            let mine = task_records(&core, chat, &task).await?;
            if mine.is_empty() {
                drop(driving);
                if self.stopped() {
                    return Ok(Driven::Over);
                }
                let project = self.project_root.clone();
                if !start_candidate(
                    &core,
                    &self.dialog_id,
                    records,
                    state,
                    settings,
                    project.as_deref(),
                )
                .await?
                {
                    return Ok(Driven::Over);
                }
                continue;
            }
            let list: Vec<StageRecord> = mine.iter().map(|r| r.record.clone()).collect();
            if next_stage(&list, effort) == Next::Over {
                return Ok(Driven::Over);
            }
            drop(driving);
            if self.stopped() {
                give_up(&core, chat, &mine, "stopped", "Stopped").await?;
                return Ok(Driven::Over);
            }
            // H07: its stage waits out the provider's rate limit, and its run
            // goes on once the wait is over: it isn't cut. After a restart,
            // its resume arms the wait again.
            let last = &mine[mine.len() - 1];
            if let Some(until) = last.record.waits_until() {
                if !autopilot::rate_limit_armed(&last.id, until) {
                    let request = settings.request(chat, StartKind::Resume);
                    autopilot::go_on_boxed(&core, &request).await?;
                }
                tokio::time::sleep(WAIT_POLL).await;
                continue;
            }
            if picked_up >= 2 {
                let why = "It couldn't go on after it was cut";
                give_up(&core, chat, &mine, "error", why).await?;
                return Ok(Driven::Over);
            }
            picked_up += 1;
            let request = settings.request(chat, StartKind::Resume);
            autopilot::go_on_boxed(&core, &request).await?;
        }
    }
}

/// How a candidate's drive ended.
enum Driven {
    /// Its records are over.
    Over,
    /// The task's run pauses (a move, the server's stop), the candidate's
    /// first.
    Paused(super::PauseRequest),
}

/*
 * ---------------------------------------------------------------------------
 * The task chat's records of its candidates' stages
 * ---------------------------------------------------------------------------
 */

/// T05, T06: the task chat's record of `stage` when its implementation was
/// its candidates': Review, Checks and Break say how each candidate's
/// went; Acceptance is the winner's. None: one implementer, whose run's
/// rows say it (`done_in_run`).
pub(super) async fn stage_record(
    core: &Core,
    dialog_id: &str,
    records: &[Recorded],
    stage: Stage,
) -> Result<Option<StageRecord>, String> {
    if !ran(records) {
        return Ok(None);
    }
    let first = &records[0].record;
    // The stage's round in the task, as `done_in_run` has it.
    let round = records.iter().filter(|r| r.record.name == stage).count() as u32 + 1;
    let base = StageRecord::ended(&first.task, stage, round, first.effort, Status::Skipped);
    if stage == Stage::Acceptance {
        let Some(w) = winner(core, dialog_id, records).await? else {
            return Ok(Some(base.with_summary("No candidate was picked")));
        };
        let theirs = w
            .records
            .iter()
            .rev()
            .find(|r| r.record.name == Stage::Acceptance);
        return Ok(Some(match theirs {
            Some(r) => StageRecord {
                candidate: None,
                reference: None,
                summary: Some(format!(
                    "{}: {}",
                    w.letter,
                    r.record
                        .summary
                        .clone()
                        .unwrap_or_else(|| status_word(r.record.status).into())
                )),
                spent: None,
                round,
                ..r.record.clone()
            },
            None => base.with_summary(format!("{}: not checked", w.letter)),
        }));
    }
    let implement = records
        .iter()
        .rev()
        .find(|r| r.record.name == Stage::Implement);
    let row = match implement.and_then(|r| r.record.reference.clone()) {
        Some(id) => row_by_id(core, dialog_id, &id).await?.map(|row| (id, row)),
        None => None,
    };
    let Some((row_id, row)) = row else {
        return Ok(Some(base.with_summary("Its candidates' records are gone")));
    };
    let mut lines = Vec::new();
    let mut done = false;
    for c in &row.candidates {
        let mine = task_records(core, &c.chat, &first.task).await?;
        let line = match mine.iter().rev().find(|r| r.record.name == stage) {
            Some(r) => {
                done |= r.record.status == Status::Done;
                r.record
                    .summary
                    .clone()
                    .unwrap_or_else(|| status_word(r.record.status).into())
            }
            None => match c.summary.as_deref() {
                Some(_) if c.status == "skipped" => "not started".into(),
                _ => "not reached".into(),
            },
        };
        lines.push(format!("{}: {line}", c.letter));
    }
    Ok(Some(StageRecord {
        status: if done { Status::Done } else { Status::Skipped },
        reference: Some(row_id),
        ..base.with_summary(lines.join("; "))
    }))
}

/*
 * ---------------------------------------------------------------------------
 * Pick (T06)
 * ---------------------------------------------------------------------------
 */

/// D14's score of a candidate, from its records and its copy.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Score {
    /// Its change isn't empty: there's something to take.
    pub changed: bool,
    /// Its run finished: no error, no Stop, no budget cut it.
    pub finished: bool,
    /// Its run ended clean (R03): with a result Autopilot lands, checked,
    /// its criteria met, no high finding open; not with a caveat (unmet,
    /// a flagged or failing check, an open finding, nothing checked).
    #[serde(default)]
    pub clean: bool,
    /// The tests written first (S04) that pass in its copy.
    pub tests_passed: usize,
    /// The project's checks that passed in its last round.
    pub checks_passed: usize,
    /// The high and medium findings its last change review left open.
    pub open_findings: usize,
    /// The lines its change adds and removes.
    pub diff_lines: usize,
}

impl Score {
    /// Better is greater: something to take, a run that finished, one that
    /// ended clean, then D14's order: the tests written first that pass,
    /// the checks that pass, fewer open findings, a smaller change.
    fn key(
        &self,
    ) -> (
        bool,
        bool,
        bool,
        usize,
        usize,
        Reverse<usize>,
        Reverse<usize>,
    ) {
        (
            self.changed,
            self.finished,
            self.clean,
            self.tests_passed,
            self.checks_passed,
            Reverse(self.open_findings),
            Reverse(self.diff_lines),
        )
    }
}

/// The candidates by their scores, the best first; equal ones in their
/// order.
pub fn ranked(scores: &[Score]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..scores.len()).collect();
    order.sort_by(|a, b| scores[*b].key().cmp(&scores[*a].key()));
    order
}

/// The winner by score; None when the best two score the same, which the
/// judge decides (D14).
pub fn decided(scores: &[Score]) -> Option<usize> {
    let order = ranked(scores);
    match order.as_slice() {
        [] => None,
        [one] => Some(*one),
        [best, next, ..] => (scores[*best].key() != scores[*next].key()).then_some(*best),
    }
}

/// The Pick's record (`meta.pick`): each candidate's score, the winner,
/// how it was decided, and the loser's diff, kept.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PickRecord {
    pub task: String,
    /// running, done, failed, stopped.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner: Option<String>,
    /// The winner's chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    /// The scores tied: the judge decided, or (`why`) couldn't.
    #[serde(default)]
    pub judged: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// The winner's change is the chat's.
    #[serde(default)]
    pub adopted: bool,
    /// Failed: why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default)]
    pub candidates: Vec<Scored>,
}

/// A candidate as the pick weighed it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Scored {
    pub letter: String,
    pub chat: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    pub score: Score,
    /// The files its change touches.
    #[serde(default)]
    pub files: Vec<String>,
    /// The loser's diff (the winner's change is the chat's now), cut to
    /// `MAX_KEPT_DIFF_CHARS`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    #[serde(default)]
    pub reduced: bool,
}

/// The task's pick, if it has one: the latest.
async fn pick_of(
    core: &Core,
    dialog_id: &str,
    task: &str,
) -> Result<Option<(String, PickRecord)>, String> {
    let id = dialog_id.to_string();
    let rows = core
        .db
        .read(move |c| store::messages_with_meta(c, &id, "pick", None))
        .await?;
    Ok(rows.iter().rev().find_map(|m| {
        let pick: PickRecord =
            serde_json::from_value(m.meta.as_ref()?.get("pick")?.clone()).ok()?;
        (pick.task == task).then(|| (m.id.clone(), pick))
    }))
}

/// T06: whether the pick a Pick stage's record names (`reference`) is done
/// with its candidates' copies: it took its winner's change, or found none
/// to take, and the copies are gone. Its failure then was its winner's
/// work's (`pick_stage`), which a Retry does again, not the pick.
pub(super) async fn took_winner(
    core: &Core,
    dialog_id: &str,
    pick: &StageRecord,
) -> Result<bool, String> {
    let Some(id) = &pick.reference else {
        return Ok(false);
    };
    let row: Option<PickRecord> = record_in_row(core, dialog_id, id, "pick").await?;
    Ok(row.is_some_and(|row| row.adopted))
}

/// The candidate the task's pick took: its letter, its chat, its records.
pub struct Winner {
    pub letter: String,
    pub chat: String,
    pub records: Vec<Recorded>,
}

/// T06: the candidate the task's pick took, if it took one.
pub async fn winner(
    core: &Core,
    dialog_id: &str,
    records: &[Recorded],
) -> Result<Option<Winner>, String> {
    if !ran(records) {
        return Ok(None);
    }
    let task = &records[0].record.task;
    let Some((_, pick)) = pick_of(core, dialog_id, task).await? else {
        return Ok(None);
    };
    let (Some(letter), Some(chat)) = (pick.winner, pick.chat) else {
        return Ok(None);
    };
    let records = task_records(core, &chat, task).await?;
    Ok(Some(Winner {
        letter,
        chat,
        records,
    }))
}

/// What a candidate's chat says of its checks and its review: the checks
/// its last round passed and whether all did, and the high and medium
/// findings its last change review left open, with its record's line.
async fn checked(core: &Core, chat: &str) -> Result<(usize, bool, usize, String), String> {
    let rows = |key: &'static str| {
        let id = chat.to_string();
        core.db
            .read(move |c| store::messages_with_meta(c, &id, key, None))
    };
    let checks = rows("verification")
        .await?
        .last()
        .and_then(|m| m.meta.as_ref()?.get("verification").cloned());
    let statuses: Vec<String> = checks
        .as_ref()
        .and_then(|v| v["checks"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|c| c["status"].as_str().map(str::to_string))
        .collect();
    let passed = statuses.iter().filter(|s| *s == "passed").count();
    let all = checks.as_ref().is_some_and(|v| v["status"] == "passed")
        || (!statuses.is_empty() && passed == statuses.len());
    let review = rows("review").await?.into_iter().rev().find_map(|m| {
        let review = m.meta.as_ref()?.get("review")?.clone();
        let reported = matches!(review["status"].as_str(), Some("clean" | "findings"));
        (review["kind"] == "change" && reported).then_some(review)
    });
    let (open, line) = match review {
        Some(review) => {
            let findings = review["findings"].as_array().cloned().unwrap_or_default();
            let serious: Vec<&Value> = findings
                .iter()
                .filter(|f| matches!(f["severity"].as_str(), Some("high" | "medium")))
                .collect();
            let line = match (review["status"].as_str(), findings.len()) {
                (Some("clean"), _) | (_, 0) => "Clean".to_string(),
                (_, n) => {
                    let titles: Vec<String> = findings
                        .iter()
                        .map(|f| {
                            format!(
                                "[{}] {}",
                                f["severity"].as_str().unwrap_or("?"),
                                f["title"].as_str().unwrap_or("")
                            )
                        })
                        .collect();
                    format!("{n} finding{}: {}", autopilot::plural(n), titles.join("; "))
                }
            };
            (serious.len(), line)
        }
        None => (0, "Not reviewed".to_string()),
    };
    Ok((passed, all, open, line))
}

impl Run {
    /// T06 (4.9): the Pick stage's run: each candidate scored by its
    /// records and its copy (D14), the judge on a tie, the winner's change
    /// made the chat's, the loser's diff kept in the record (`meta.pick`).
    /// Its outcome is the winner's implementation's, so the change lands
    /// as that run's would have (landing.rs); "stopped" when stopped.
    pub(super) async fn pick_stage(&mut self) -> Result<String, String> {
        if self.stage.is_none() {
            return Ok("done".into());
        }
        let (core, main) = (self.core.clone(), self.dialog_id.clone());
        let Some((task, records)) = autopilot::current_task(&core, &main).await? else {
            return Ok("done".into());
        };
        let failed = |outcome: Option<String>, summary: String, reference: String| {
            move |r: &mut StageRecord| {
                r.status = Status::Failed;
                r.outcome = outcome;
                r.summary = Some(summary);
                r.reference = Some(reference);
            }
        };
        let implement = records
            .iter()
            .rev()
            .find(|r| r.record.name == Stage::Implement)
            .cloned();
        let row = match implement.as_ref().and_then(|r| r.record.reference.clone()) {
            Some(id) => row_by_id(&core, &main, &id).await?,
            None => None,
        };
        let Some(row) = row else {
            let why = "No candidates to pick from";
            self.end_own_stage(Status::Skipped, why, None).await?;
            return Ok("done".into());
        };
        // A pick a restart cut goes on in its record, from the winner's
        // change taken if it was.
        let (pick_row, mut pick) = match pick_of(&core, &main, &task).await? {
            Some((id, pick)) if pick.status == "running" => (id, pick),
            _ => {
                let pick = PickRecord {
                    task: task.clone(),
                    status: "running".into(),
                    ..PickRecord::default()
                };
                let id = autopilot::add_record_row(&core, &main, "pick", json!(pick)).await?;
                (id, pick)
            }
        };
        if !pick.adopted {
            // Decided before a crash: the winner is taken, never scored again
            // (its copy may be gone already, taken).
            let work = match pick.winner {
                Some(_) => self.take_winner(&mut pick, &pick_row).await?,
                None => {
                    self.score_and_take(&records, &row, &mut pick, &pick_row)
                        .await?
                }
            };
            match work {
                PickWork::Stopped => {
                    pick.status = "stopped".into();
                    save_row(&core, &main, &pick_row, "pick", json!(pick)).await?;
                    return Ok("stopped".into());
                }
                PickWork::Failed(why) => {
                    pick.status = "failed".into();
                    pick.reason = Some(why.clone());
                    save_row(&core, &main, &pick_row, "pick", json!(pick)).await?;
                    let error = Some("error".into());
                    self.end_own_stage_with(failed(error, why, pick_row))
                        .await?;
                    return Ok("error".into());
                }
                PickWork::Taken => {}
            }
        }
        // The losers' copies go; their diffs are in the record.
        let copies = copies_here(&core);
        for c in &row.candidates {
            if Some(&c.chat) != pick.chat.as_ref() {
                copies.discard(main.clone(), c.chat.clone()).await;
            }
            core.agents.drop_candidate(&c.chat);
        }
        let letter = pick.winner.clone().unwrap_or_default();
        let won = pick.candidates.iter().find(|c| c.letter == letter).cloned();
        let outcome = won
            .as_ref()
            .and_then(|c| c.outcome.clone())
            .unwrap_or_else(|| "done".into());
        // The task's implementation is the winner's now: its outcome, and
        // its failure, if a stage of its failed, is the task's.
        let chat = pick.chat.clone().unwrap_or_default();
        let theirs = task_records(&core, &chat, &task).await?;
        let theirs_failed = theirs
            .iter()
            .find(|r| r.record.status == Status::Failed && r.record.name != Stage::Acceptance)
            .map(|r| (r.record.outcome.clone(), r.record.summary.clone()));
        if let Some(implement) = &implement {
            let (outcome, letter) = (outcome.clone(), letter.clone());
            end_stage(&core, &main, &implement.id, |r| {
                r.outcome = Some(outcome);
                r.summary = Some(format!("Candidate {letter}'s, picked"));
            })
            .await?;
        }
        pick.status = "done".into();
        save_row(&core, &main, &pick_row, "pick", json!(pick)).await?;
        let summary = pick_summary(&pick);
        match theirs_failed {
            Some((outcome, why)) => {
                let why = why.unwrap_or_else(|| "a stage failed".into());
                let summary = format!("{summary}; {letter}: {why}");
                self.end_own_stage_with(failed(outcome, summary, pick_row))
                    .await?
            }
            None => {
                self.end_own_stage(Status::Done, summary, Some(pick_row))
                    .await?
            }
        }
        super::log::write(
            &core,
            json!({
                "event": "pick",
                "dialogId": main,
                "winner": letter,
                "judged": pick.judged,
                "outcome": outcome,
            }),
        );
        Ok(outcome)
    }

    /// The pick's work up to the winner's change taken: the scores, the
    /// judge on a tie, the loser's diff kept, the winner adopted.
    async fn score_and_take(
        &mut self,
        records: &[Recorded],
        row: &CandidatesRecord,
        pick: &mut PickRecord,
        pick_row: &str,
    ) -> Result<PickWork, String> {
        let (core, main) = (self.core.clone(), self.dialog_id.clone());
        let task = records[0].record.task.clone();
        let copies = copies_here(&core);
        let tests = autopilot::task_tests(&core, &main).await?.files;
        let mut scored = Vec::new();
        let mut diffs = Vec::new();
        let mut reviews = Vec::new();
        for c in &row.candidates {
            if self.stopped() {
                return Ok(PickWork::Stopped);
            }
            let started = !task_records(&core, &c.chat, &task).await?.is_empty();
            // A copy that can't be opened, or a change that can't be read,
            // fails the pick, its copies kept: taken as none, it could be
            // thrown away (R03).
            let folder = match started {
                true => match (copies)
                    .open(main.clone(), c.chat.clone(), self.project_root.clone())
                    .await
                {
                    Ok(folder) => Some(folder),
                    Err(why) => {
                        return Ok(PickWork::Failed(format!(
                            "Candidate {}'s working copy couldn't be opened: {why}",
                            c.letter
                        )))
                    }
                },
                false => None,
            };
            let change = match &folder {
                Some(_) => match copies.change(main.clone(), c.chat.clone()).await {
                    Ok(change) => change,
                    Err(why) => {
                        return Ok(PickWork::Failed(format!(
                            "Candidate {}'s change couldn't be read: {why}",
                            c.letter
                        )))
                    }
                },
                None => Vec::new(),
            };
            let rendered = render_diff(&change, usize::MAX);
            let (checks_passed, all_passed, open_findings, review) =
                checked(&core, &c.chat).await?;
            let mut tests_passed = 0;
            if let (Some(folder), false) = (&folder, change.is_empty()) {
                for file in &tests {
                    let passes = match self.passes_in(&c.chat, folder, file).await {
                        Some(passes) => passes,
                        // No way to run it alone: as its checks went.
                        None => all_passed,
                    };
                    tests_passed += usize::from(passes);
                }
            }
            let finished = c.outcome.as_deref().is_some_and(autopilot::finished);
            let score = Score {
                changed: !change.is_empty(),
                finished,
                clean: c.outcome.as_deref().is_some_and(autopilot::clean),
                tests_passed,
                checks_passed,
                open_findings,
                diff_lines: rendered.changed_lines,
            };
            scored.push(Scored {
                letter: c.letter.clone(),
                chat: c.chat.clone(),
                outcome: c.outcome.clone(),
                score,
                files: rendered.files.clone(),
                diff: None,
                reduced: false,
            });
            diffs.push(rendered.text);
            reviews.push(review);
        }
        let scores: Vec<Score> = scored.iter().map(|s| s.score).collect();
        pick.candidates = scored.clone();
        save_row(&core, &main, pick_row, "pick", json!(pick)).await?;
        let at = match decided(&scores) {
            Some(at) => at,
            None => {
                pick.judged = true;
                let tied: Vec<usize> = {
                    let order = ranked(&scores);
                    let best = scores[order[0]].key();
                    order
                        .into_iter()
                        .filter(|i| scores[*i].key() == best)
                        .collect()
                };
                let judged = match scores[tied[0]].changed {
                    // Nothing to take either way: the first.
                    false => Err("neither candidate changed anything".to_string()),
                    true => {
                        self.judge_candidates(&task, &scored, &tied, &diffs, &reviews)
                            .await?
                    }
                };
                match judged {
                    Ok(Some(letter)) => scored
                        .iter()
                        .position(|s| s.letter == letter)
                        .unwrap_or(tied[0]),
                    Ok(None) => return Ok(PickWork::Stopped),
                    Err(why) => {
                        pick.why = Some(format!("{why}; the first candidate is taken"));
                        tied[0]
                    }
                }
            }
        };
        if self.stopped() {
            return Ok(PickWork::Stopped);
        }
        // The losers' diffs, kept.
        for (i, s) in pick.candidates.iter_mut().enumerate() {
            if i != at && !diffs[i].is_empty() {
                let kept = char_prefix(&diffs[i], MAX_KEPT_DIFF_CHARS);
                s.reduced = kept.len() < diffs[i].len();
                s.diff = Some(kept.to_string());
            }
        }
        let winner = &scored[at];
        pick.winner = Some(winner.letter.clone());
        pick.chat = Some(winner.chat.clone());
        save_row(&core, &main, pick_row, "pick", json!(pick)).await?;
        self.take_winner(pick, pick_row).await
    }

    /// The pick's winner's change becomes the chat's, its copy gone (with no
    /// change, just gone), and the record says so (`adopted`). A pick a
    /// crash cut after it decided goes on here (R03): a winner's copy that's
    /// gone was taken already, and the others' were never scored again.
    async fn take_winner(&self, pick: &mut PickRecord, pick_row: &str) -> Result<PickWork, String> {
        let (core, main) = (self.core.clone(), self.dialog_id.clone());
        let (Some(letter), Some(chat)) = (pick.winner.clone(), pick.chat.clone()) else {
            return Ok(PickWork::Failed("The pick has no winner to take".into()));
        };
        let copies = copies_here(&core);
        let changed = (pick.candidates.iter()).any(|c| c.letter == letter && c.score.changed);
        if changed && copies.kept(main.clone(), chat.clone()).await {
            if let Err(why) = copies.adopt(main.clone(), chat.clone()).await {
                return Ok(PickWork::Failed(format!(
                    "Candidate {letter}'s change couldn't be taken: {why}"
                )));
            }
        } else {
            copies.discard(main.clone(), chat.clone()).await;
        }
        #[cfg(test)]
        if autopilot::crashes(&main, autopilot::Crash::AfterAdopt) {
            // The process "dies" here: nothing more happens in this run.
            std::future::pending::<()>().await;
        }
        pick.adopted = true;
        save_row(&core, &main, pick_row, "pick", json!(pick)).await?;
        Ok(PickWork::Taken)
    }

    /// Whether `file`'s tests pass alone in a candidate's copy (`root`),
    /// with its chat's environment. None: there's no way to run it alone,
    /// or it timed out.
    async fn passes_in(&self, chat: &str, root: &str, file: &str) -> Option<bool> {
        let command = super::breaker::alone(Path::new(root), file)?;
        let stop = self.stop.clone();
        let overlay = crate::overlay::chats::get(chat);
        let queued = crate::command_tool::Queued {
            kind: crate::workload::JobKind::Check {
                owner: self.dialog_id.clone(),
            },
            note: Box::new(|_| {}),
            env: overlay
                .as_ref()
                .map(|o| o.command_env())
                .unwrap_or_default(),
            confine: overlay.as_ref().and_then(|o| o.confine()),
            whole_output: false,
        };
        let out = crate::command_tool::run_command(
            root.to_string(),
            command,
            Some(super::verify::check_timeout_ms(&self.dialog_id)),
            move || stop.is_set(),
            Some(queued),
        )
        .await
        .ok()?;
        if out.timed_out || out.stopped {
            return None;
        }
        Some(out.exit_code == Some(0))
    }

    /// The judge on a tie (D14): the tied candidates' diffs, the criteria,
    /// their reviews. Ok(Some): the letter it picked; Ok(None): stopped;
    /// Err: why it didn't pick one.
    async fn judge_candidates(
        &mut self,
        task: &str,
        scored: &[Scored],
        tied: &[usize],
        diffs: &[String],
        reviews: &[String],
    ) -> Result<Result<Option<String>, String>, String> {
        let config = self.config().subagents;
        if config.kind(Kind::Judge).is_none() {
            return Ok(Err("the judge is off".into()));
        }
        if let Some(why) = self.core.agents.over_budget(&self.dialog_id) {
            return Ok(Err(format!("no judge: over the task's budget ({why})")));
        }
        let plan = match autopilot::task_plan_file(&self.core, &self.dialog_id).await? {
            Some(file) => std::fs::read_to_string(file).unwrap_or_default(),
            None => String::new(),
        };
        let criteria = super::plan::criteria_section(&plan).unwrap_or_else(|| "None given.".into());
        let request = {
            let id = task.to_string();
            self.core
                .db
                .read(move |c| store::get_message_body(c, &id))
                .await
                .unwrap_or_default()
        };
        let weighed: Vec<judge::Weighed> = tied
            .iter()
            .map(|i| judge::Weighed {
                letter: &scored[*i].letter,
                diff: char_prefix(&diffs[*i], JUDGE_DIFF_CHARS),
                review: &reviews[*i],
            })
            .collect();
        let letters: Vec<&str> = tied.iter().map(|i| scored[*i].letter.as_str()).collect();
        let message = judge::candidates_brief(&request, &criteria, &weighed);
        let mut row = new_message("assistant", "");
        let mut record = json!({ "role": "candidates", "status": "running", "offered": letters });
        row.meta = Some(json!({ "judge": record.clone() }));
        let row_id = row.id.clone();
        self.append(vec![row]).await?;
        let started = Instant::now();
        let outcome = self
            .run_ide_subagent(
                Kind::Judge,
                &row_id,
                "Judge the candidates' changes",
                message,
            )
            .await;
        record["ms"] = json!(started.elapsed().as_millis() as u64);
        let judged = match subagent::outcome_of(&mut record, outcome) {
            SubOutcome::Stopped => None,
            SubOutcome::Skipped(skip) => Some(Err(super::planning::judge_skip_reason(&skip))),
            SubOutcome::Report(r) => {
                let sections = subagent::sections(&r.text);
                if let Some(why) = sections.get("Why") {
                    record["why"] = json!(char_prefix(why.trim(), judge::MAX_RECORDED_WHY_CHARS));
                }
                Some(
                    sections
                        .get("Verdict")
                        .and_then(|v| judge::verdict_of(v, &letters))
                        .ok_or_else(|| "its verdict names neither candidate".to_string()),
                )
            }
        };
        let result = match judged {
            None => {
                record["status"] = json!("stopped");
                Ok(None)
            }
            Some(Err(why)) => {
                record["status"] = json!("skipped");
                record["reason"] = json!(why);
                Err(format!("the judge didn't decide: {why}"))
            }
            Some(Ok(letter)) => {
                record["status"] = json!("done");
                record["choice"] = json!(letter);
                Ok(Some(letter))
            }
        };
        self.update_meta(&row_id, json!({ "judge": record.clone() }))
            .await?;
        super::log::write(
            &self.core,
            json!({
                "event": "judge",
                "dialogId": self.dialog_id,
                "role": "candidates",
                "status": record["status"],
                "choice": record["choice"],
                "ms": record["ms"],
            }),
        );
        Ok(result)
    }
}

/// How the pick's work went: the winner's change taken, a Stop, or why
/// it failed.
enum PickWork {
    Taken,
    Stopped,
    Failed(String),
}

/// The Pick's line: the winner, how, and each candidate's counts.
pub fn pick_summary(pick: &PickRecord) -> String {
    let letter = pick.winner.as_deref().unwrap_or("?");
    let how = match (pick.judged, &pick.why) {
        (true, Some(_)) => "tied; the first taken",
        (true, None) => "tied; the judge's pick",
        (false, _) => "by score",
    };
    let counts = pick
        .candidates
        .iter()
        .map(|c| {
            let s = &c.score;
            // How a run that didn't end clean ended, which outranks the counts.
            let ended = match (s.clean, &c.outcome) {
                (false, Some(outcome)) => format!(" ({outcome})"),
                _ => String::new(),
            };
            format!(
                "{}{ended}: {} test{} pass, {} check{} pass, {} finding{} open, {} line{}",
                c.letter,
                s.tests_passed,
                autopilot::plural(s.tests_passed),
                s.checks_passed,
                autopilot::plural(s.checks_passed),
                s.open_findings,
                autopilot::plural(s.open_findings),
                s.diff_lines,
                autopilot::plural(s.diff_lines),
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!("Candidate {letter}, {how} ({counts})")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(tests: usize, checks: usize, findings: usize, lines: usize) -> Score {
        Score {
            changed: true,
            finished: true,
            clean: true,
            tests_passed: tests,
            checks_passed: checks,
            open_findings: findings,
            diff_lines: lines,
        }
    }

    /// D14: the tests written first that pass decide first, then the
    /// checks, then fewer open findings, then the smaller change; before
    /// all of them, a change to take, and a run that finished.
    #[test]
    fn the_score_orders_by_tests_then_checks_then_findings_then_size() {
        let winner = |a: Score, b: Score| decided(&[a, b]);
        // More tests passed wins, whatever else.
        assert_eq!(winner(score(2, 0, 9, 900), score(1, 5, 0, 1)), Some(0));
        // The same tests: more checks.
        assert_eq!(winner(score(1, 1, 0, 1), score(1, 2, 5, 900)), Some(1));
        // The same checks: fewer findings left open.
        assert_eq!(winner(score(1, 2, 1, 1), score(1, 2, 0, 900)), Some(1));
        // The same findings: the smaller change.
        assert_eq!(winner(score(1, 2, 0, 40), score(1, 2, 0, 12)), Some(1));
        // The same on every count: a tie, for the judge.
        assert_eq!(winner(score(1, 2, 0, 12), score(1, 2, 0, 12)), None);
        // A change beats none; a finished run beats one that didn't.
        let empty = Score {
            changed: false,
            ..score(9, 9, 0, 0)
        };
        assert_eq!(winner(empty, score(0, 0, 3, 50)), Some(1));
        let cut = Score {
            finished: false,
            ..score(3, 3, 0, 1)
        };
        assert_eq!(winner(cut, score(0, 0, 0, 80)), Some(1));
        // R03: of finished runs, one that ended clean beats one that ended
        // with a caveat (a criterion unmet), more tests or a smaller change
        // notwithstanding.
        let caveat = Score {
            clean: false,
            ..score(2, 3, 0, 5)
        };
        assert_eq!(winner(caveat, score(1, 1, 0, 50)), Some(1));
        // The order, best first; equal ones keep theirs.
        assert_eq!(
            ranked(&[score(0, 1, 0, 5), score(2, 0, 0, 5), score(0, 1, 0, 5)]),
            [1, 0, 2]
        );
        assert_eq!(decided(&[score(1, 1, 1, 1)]), Some(0));
        assert_eq!(decided(&[]), None);
    }

    #[test]
    fn the_picks_line_names_the_winner_how_and_each_ones_counts() {
        let mut pick = PickRecord {
            task: "t".into(),
            status: "done".into(),
            winner: Some("b".into()),
            ..PickRecord::default()
        };
        for (letter, s) in [("a", score(1, 2, 1, 30)), ("b", score(1, 2, 0, 1))] {
            pick.candidates.push(Scored {
                letter: letter.into(),
                chat: format!("chat-{letter}"),
                outcome: Some("verified".into()),
                score: s,
                files: Vec::new(),
                diff: None,
                reduced: false,
            });
        }
        assert_eq!(
            pick_summary(&pick),
            "Candidate b, by score (a: 1 test pass, 2 checks pass, 1 finding open, 30 lines; b: 1 test pass, 2 checks pass, 0 findings open, 1 line)"
        );
        // A run that didn't end clean says how it ended.
        pick.candidates[0].score.clean = false;
        pick.candidates[0].outcome = Some("unmet".into());
        assert!(
            pick_summary(&pick).contains("(a (unmet): 1 test pass,"),
            "{}",
            pick_summary(&pick)
        );
        pick.judged = true;
        assert!(pick_summary(&pick).starts_with("Candidate b, tied; the judge's pick ("));
        pick.why = Some("the judge is off; the first candidate is taken".into());
        assert!(pick_summary(&pick).starts_with("Candidate b, tied; the first taken ("));
    }

    #[test]
    fn a_candidates_record_reads_as_written() {
        let written = json!({
            "task": "t",
            "round": 1,
            "status": "running",
            "candidates": [
                { "letter": "a", "chat": "c1", "status": "done", "outcome": "verified", "summary": "1 check passed" },
                { "letter": "b", "chat": "c2", "status": "waiting" },
            ],
        });
        let record: CandidatesRecord = serde_json::from_value(written.clone()).unwrap();
        assert_eq!(record.candidates[1].chat, "c2");
        assert_eq!(serde_json::to_value(&record).unwrap(), written);
        let single: CandidatesRecord = serde_json::from_value(
            json!({ "task": "t", "status": "single", "reason": "no copies" }),
        )
        .unwrap();
        assert_eq!((single.round, single.candidates.len()), (1, 0));
    }
}
