//! The stage records as rows of the chat (D3): read, added, ended, and the
//! spend each run keeps in its record as it goes (U04); the chat's current
//! task, and where a stage's run starts its history (D2).

use super::stage::{
    add_spent, latest_task, next_after, record_of, recorded, records_of, total_spent, Next,
    Recorded, Stage, StageRecord, StageRun, Status,
};
use crate::agent_run::budget::Spent;
use crate::agent_run::messages;
use crate::chat_db::store::{self, MessagePatch, MessageRow, NewMessage};
use crate::core::Core;
use serde::Serialize;
use serde_json::{json, Value};

/// The chat's stage records, every task's, oldest first: read from the
/// database, so they're whatever the chat holds now, however long its runs.
pub async fn load(core: &Core, dialog_id: &str) -> Result<Vec<Recorded>, String> {
    let id = dialog_id.to_string();
    let rows = core
        .db
        .read(move |c| store::messages_with_meta(c, &id, "stage", None))
        .await?;
    Ok(recorded(&rows))
}

/// Adds a stage record at the end of the chat, and shows it: the row's id.
pub async fn add_stage(
    core: &Core,
    dialog_id: &str,
    record: &StageRecord,
) -> Result<String, String> {
    let id = add_record_row(core, dialog_id, "stage", json!(record)).await?;
    log_stage(core, dialog_id, record, false);
    Ok(id)
}

/// Changes the stage record in row `id` (its end: the status, outcome,
/// what it produced, its summary), and shows it.
pub async fn end_stage(
    core: &Core,
    dialog_id: &str,
    id: &str,
    change: impl FnOnce(&mut StageRecord),
) -> Result<StageRecord, String> {
    let record = patch_stage(core, dialog_id, id, change).await?;
    log_stage(core, dialog_id, &record, true);
    Ok(record)
}

/// `end_stage` with what the run spent so far (`so_far`) on the record, in
/// the same write (U04): the stage's end puts no less on its record than
/// its run had spent, so a reader never finds it over and underpaid.
pub(in crate::agent_run) async fn end_stage_spent(
    core: &Core,
    dialog_id: &str,
    id: &str,
    so_far: Option<Spent>,
    change: impl FnOnce(&mut StageRecord),
) -> Result<StageRecord, String> {
    end_stage(core, dialog_id, id, |record| {
        change(record);
        if so_far.is_some() {
            record.spending = so_far;
        }
    })
    .await
}

/// The log's line for a stage record as it's added, or as it ends (with
/// its outcome).
pub(super) fn log_stage(core: &Core, dialog_id: &str, record: &StageRecord, ended: bool) {
    let mut line = json!({
        "event": "stage",
        "dialogId": dialog_id,
        "task": record.task,
        "stage": record.name.id(),
        "status": record.status,
        "round": record.round,
    });
    if ended {
        line["outcome"] = json!(record.outcome);
    }
    crate::agent_run::log::write(core, line);
}

/// `end_stage` without its line in the log: for what changes as a stage
/// goes (U04's spend so far).
pub(super) async fn patch_stage(
    core: &Core,
    dialog_id: &str,
    id: &str,
    change: impl FnOnce(&mut StageRecord),
) -> Result<StageRecord, String> {
    let (row, mut record) = record_row(core, dialog_id, id).await?;
    change(&mut record);
    let mut meta = row
        .meta
        .clone()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    meta["stage"] = json!(record);
    save_meta(core, dialog_id, id, meta).await?;
    Ok(record)
}

impl crate::agent_run::run::Run {
    /// Ends the record of the stage this run carries out (`end_stage`) as
    /// `change` says: the work of a stage the IDE does itself ends it so.
    /// A run of no stage has none.
    pub(in crate::agent_run) async fn end_own_stage_with(
        &self,
        change: impl FnOnce(&mut StageRecord),
    ) -> Result<(), String> {
        match &self.stage {
            // U04: what the run spent so far ends with its record, in one
            // write: its stage never reads as over with less on it than
            // the run spent.
            Some(stage) => {
                let so_far = self.spent_so_far();
                end_stage_spent(&self.core, &self.dialog_id, &stage.record, so_far, change)
                    .await
                    .map(drop)
            }
            None => Ok(()),
        }
    }

    /// `end_own_stage_with` as most ends go: the stage's status, its line,
    /// and the record it produced.
    pub(in crate::agent_run) async fn end_own_stage(
        &self,
        status: Status,
        summary: impl Into<String>,
        reference: Option<String>,
    ) -> Result<(), String> {
        let summary = summary.into();
        self.end_own_stage_with(|r| {
            r.status = status;
            r.summary = Some(summary);
            r.reference = reference;
        })
        .await
    }
}

/// U04: what a stage's run spent so far, kept in its record as it goes,
/// so that a crash keeps it.
pub(in crate::agent_run) async fn keep_spending(
    core: &Core,
    dialog_id: &str,
    record: &str,
    so_far: Spent,
) -> Result<(), String> {
    patch_stage(core, dialog_id, record, |r| r.spending = Some(so_far))
        .await
        .map(drop)
}

/// U04: a run's spend is its stage record's from its end, all of it
/// (`total`), in place of what the run kept of it as it went. Told before
/// the run's end is, so the record a reader finds once no run of the chat
/// goes is whole.
pub(in crate::agent_run) async fn settle_spent(
    core: &Core,
    dialog_id: &str,
    record: &str,
    total: Spent,
) -> Result<(), String> {
    patch_stage(core, dialog_id, record, |r| {
        r.spent = Some(r.spent.unwrap_or_default().plus(total));
        r.spending = None;
    })
    .await
    .map(drop)
}

/// U04: what a run a crash cut kept of its spend as it went is its stage
/// record's from here, before the stage's next run keeps its own.
pub(in crate::agent_run) async fn keep_cut_spending(
    core: &Core,
    dialog_id: &str,
    record: &str,
) -> Result<(), String> {
    if record_in(core, dialog_id, record).await?.spending.is_none() {
        return Ok(());
    }
    patch_stage(core, dialog_id, record, |r| {
        let cut = r.spending.take().unwrap_or_default();
        r.spent = Some(r.spent.unwrap_or_default().plus(cut));
    })
    .await
    .map(drop)
}

/// The stage record in row `id` of the chat, with its row.
pub(super) async fn record_row(
    core: &Core,
    dialog_id: &str,
    id: &str,
) -> Result<(MessageRow, StageRecord), String> {
    let ids = vec![id.to_string()];
    let row = core
        .db
        .read(move |c| store::messages_by_id(c, &ids))
        .await?
        .into_iter()
        .next()
        .filter(|row| row.dialog_id == dialog_id)
        .ok_or_else(|| format!("stage record {id} isn't in chat {dialog_id}"))?;
    let record = record_of(&row).ok_or_else(|| format!("message {id} holds no stage record"))?;
    Ok((row, record))
}

/// The stage record in row `id` of the chat.
pub(super) async fn record_in(
    core: &Core,
    dialog_id: &str,
    id: &str,
) -> Result<StageRecord, String> {
    Ok(record_row(core, dialog_id, id).await?.1)
}

/// A record of the IDE's at the end of the chat (`meta.<key>`), shown:
/// an empty assistant message, which is never sent to the model. Its row.
pub(in crate::agent_run) async fn add_record_row(
    core: &Core,
    dialog_id: &str,
    key: &str,
    value: Value,
) -> Result<String, String> {
    let mut row = crate::agent_run::run::new_message("assistant", "");
    row.meta = Some(json!({ key: value }));
    let id = row.id.clone();
    append(core, dialog_id, row).await?;
    Ok(id)
}

/// Row `id`'s meta, stored as `meta` now, and shown.
pub(in crate::agent_run) async fn save_meta(
    core: &Core,
    dialog_id: &str,
    id: &str,
    meta: Value,
) -> Result<(), String> {
    let (row_id, patch) = (
        id.to_string(),
        MessagePatch {
            meta: Some(meta.clone()),
            ..Default::default()
        },
    );
    core.db
        .write(move |c| store::update_message(c, &row_id, &patch))
        .await?;
    core.emit(crate::agent_run::events::AgentEvent::MessagePatched {
        dialog_id: dialog_id.to_string(),
        message_id: id.to_string(),
        patch: crate::agent_run::events::Patch {
            meta: Some(meta),
            ..Default::default()
        },
    });
    Ok(())
}

/// Stores a row at the end of the chat and shows it.
pub(in crate::agent_run) async fn append(
    core: &Core,
    dialog_id: &str,
    row: NewMessage,
) -> Result<(), String> {
    let (id, stored) = (dialog_id.to_string(), vec![row.clone()]);
    let seqs = core
        .db
        .write(move |c| store::append_messages(c, &id, &stored))
        .await?;
    core.emit(crate::agent_run::events::AgentEvent::MessagesAdded {
        dialog_id: dialog_id.to_string(),
        messages: vec![crate::agent_run::events::out_message(
            dialog_id, seqs[0], &row, false,
        )],
    });
    Ok(())
}

/// The chat's latest task, with its records, while it's the chat's current
/// request: no message of the user's came after it. A new message takes
/// over from a task that's still unfinished; its records stay as they were.
/// One that never started (marked failed, its draft given back) takes over
/// from nothing, as the strip has it (autopilot.ts `currentTask`).
pub async fn current_task(
    core: &Core,
    dialog_id: &str,
) -> Result<Option<(String, Vec<Recorded>)>, String> {
    let all = load(core, dialog_id).await?;
    let Some(task) = latest_task(&all).map(str::to_string) else {
        return Ok(None);
    };
    let records: Vec<Recorded> = records_of(&all, &task).into_iter().cloned().collect();
    let (id, since) = (dialog_id.to_string(), records[0].seq);
    let later = core
        .db
        .read(move |c| store::role_messages_after(c, &id, "user", since))
        .await?;
    if later.iter().any(|m| !messages::is_harness(m) && !m.failed) {
        return Ok(None);
    }
    Ok(Some((task, records)))
}

/// A stage record and its row, as the app shows them.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StageRow {
    pub id: String,
    pub stage: StageRecord,
}

/// The records of the chat's task while it's the chat's current request,
/// oldest first, for the strip above the input; None when there's none. A
/// task's runs can be longer than the page of messages the app holds.
pub async fn task_stages(core: &Core, dialog_id: &str) -> Result<Option<Vec<StageRow>>, String> {
    Ok(current_task(core, dialog_id).await?.map(|(_, records)| {
        records
            .into_iter()
            .map(|r| StageRow {
                id: r.id,
                stage: r.record,
            })
            .collect()
    }))
}

/// What the chat's current task spent so far, by its records (U04): its
/// stages' runs, the one under way's part included where a move or an
/// upgrade paused it. A candidate's chat's is its task's (T05): its task
/// chat's records, whose implementation's has each candidate's spending
/// once it's over, and the candidate's own until then.
pub async fn task_spent(core: &Core, dialog_id: &str) -> Result<Spent, String> {
    let chat = task_chat(core, dialog_id).await?;
    let Some((task, records)) = current_task(core, &chat).await? else {
        return Ok(Spent::default());
    };
    let mut spent = total_spent(records.iter().map(|r| &r.record));
    if chat != dialog_id {
        let all = load(core, dialog_id).await?;
        spent = add_spent(
            spent,
            records_of(&all, &task).into_iter().map(|r| &r.record),
        );
    }
    Ok(spent)
}

/// T05: the chat whose task a chat's runs carry out: a candidate's task
/// chat, else the chat itself.
pub(in crate::agent_run) async fn task_chat(
    core: &Core,
    dialog_id: &str,
) -> Result<String, String> {
    Ok(crate::agent_run::candidates::main_of(core, dialog_id)
        .await?
        .unwrap_or_else(|| dialog_id.to_string()))
}

/// D2: where a stage's run starts its history, a context fresh from the
/// task's artifacts. The implementation, and the delivery's report (written
/// in the implementer's context, S09), see the task's request (its
/// message, which the repository map goes on) and then their own stage's
/// rows, from its first round's record: not the plan stage's conversation,
/// whose result is the plan the implementation's request carries, nor the
/// chat's earlier tasks. Its rounds see each other's. A candidate's chat
/// holds no request of the task's: its implementation's request carries
/// the plan. The plan stage keeps the chat as it was, where the request is
/// read. None: the chat's whole history.
pub(in crate::agent_run) async fn history_window(
    core: &Core,
    dialog_id: &str,
    stage: Option<&StageRun>,
) -> Result<Option<crate::chat_db::history::Window>, String> {
    let Some(stage) = stage.filter(|s| s.stage.fresh_context()) else {
        return Ok(None);
    };
    let all = load(core, dialog_id).await?;
    let mine = records_of(&all, &stage.task);
    let from = mine
        .iter()
        .find(|r| r.record.name == Stage::Implement)
        .or_else(|| mine.iter().find(|r| r.id == stage.record))
        .map(|r| r.seq);
    Ok(from.map(|from_seq| crate::chat_db::history::Window {
        from_seq,
        keep: Some(stage.task.clone()),
    }))
}

/// Whether the chat has a task that isn't over: a Resume goes on with it.
pub async fn unfinished(core: &Core, dialog_id: &str) -> Result<bool, String> {
    let Some((_, records)) = current_task(core, dialog_id).await? else {
        return Ok(false);
    };
    Ok(next_after(&records) != Next::Over)
}
