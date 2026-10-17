//! Autopilot (docs/design/autopilot-plan.md): a chat mode in which one
//! message is a whole task. The agent plans it, the plan is reviewed, the
//! plan is implemented and the change checked, and the result delivered,
//! with nobody at the keyboard.
//!
//! A task is a pipeline of runs in one chat (D2), not one long run. Its
//! first run is a Plan-mode run, started by the user's message.
//!
//! Every stage leaves a record (D3): an empty assistant message with
//! `meta.stage`, which is never sent to the model (empty assistant
//! messages aren't) and goes wherever the chat's rows go, the mirror of a
//! remote chat included. The pipeline's state *is* its records: what ran,
//! how it went, and so what comes next (`next_stage`). Nothing else holds
//! it, so an app that reopens, or a daemon that restarts, reads the rows
//! and goes on (A03).
//!
//! The driver (`after_run`) runs where a run's end is told, after its
//! landing: it ends the stage the run carried out, then records and starts
//! the stages that follow, until one is a run of its own (the
//! implementation) or the task is over. Stages a run did inside itself (a
//! plan's review in `submit_plan`, the change review and the checks at the
//! end of the implementing run) are recorded from what that run left in
//! the chat; stages not built yet are recorded as skipped. `resume` picks a
//! task up where a crash, a restart or a closed app left it.
//!
//! Its parts: `stage`, the stages and their records (pure); `outcome`,
//! what a run's outcome means for its stage and task; `config`, what a
//! task's runs follow; `rows`, the records as rows of the chat;
//! `artifacts`, what stages leave for later ones; `driver`, from a run's
//! end to the next stage; `rate_limit` (H07); `inside`, the stages a run
//! does inside itself; `delivery`, the last stage and the report's facts.
//! A new stage says who carries it out in `Stage::carrier`, and the
//! compiler points at the matches that need its case.

mod artifacts;
mod config;
mod delivery;
mod driver;
mod inside;
mod outcome;
mod rate_limit;
mod rows;
mod stage;
#[cfg(test)]
mod tests;

pub(super) use artifacts::implementation_message;
pub use artifacts::{task_plan_file, task_tests, tests_line, tests_row, TaskTests};
pub use config::{
    needed_kinds, permissions_for, run_config, subagent_config, without_questions,
    DAEMON_FOLDER_REFUSED, MODE, NO_QUESTIONS,
};
pub(super) use delivery::push_result;
pub use delivery::{
    plan_title, report_facts, CheckLine, DeliverAsk, Deliverer, Delivering, Delivery, Pushed,
    ReportFacts,
};
pub(super) use driver::{
    after_run, drive_lock, driver_turn, go_on_boxed, not_started_why, try_drive_lock,
};
#[cfg(test)]
pub(super) use driver::{crash_at, crashes, Crash};
pub use driver::{resume, stage_for, Settings, DRIVER_BUSY};
pub(super) use outcome::plural;
pub use outcome::{clean, finished};
#[cfg(test)]
pub(super) use rate_limit::forget_rate_limit_wait;
pub(super) use rate_limit::{rate_limit_armed, stop_waiting};
pub use rovik_protocol::autopilot::{AutopilotConfig, Budget, Effort};
pub(super) use rows::{
    add_record_row, append, history_window, keep_cut_spending, keep_spending, save_meta,
};
pub use rows::{
    add_stage, current_task, end_stage, load, task_spent, task_stages, unfinished, StageRow,
};
pub(super) use stage::add_spent;
pub use stage::{
    latest_task, next_after, next_stage, pipeline, record_of, recorded, records_of, task_state,
    total_spent, Carrier, Next, RateLimitWait, Recorded, Stage, StageRecord, StageRun, Status,
};
