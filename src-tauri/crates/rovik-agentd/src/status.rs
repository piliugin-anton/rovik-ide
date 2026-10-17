//! `status [--data DIR] [--json]` (docs/design/autopilot-plan.md H08):
//! this user's chats on the server, from state.json, each with how its
//! last run ended, the run going now, and its latest Autopilot task from
//! the chat database: its latest stage and what its runs spent. Read while
//! the daemon runs, without its socket: the database opened to read only.
//! The hosts page asks the daemon for the same, of its computer's chats
//! (`TaskStatus`).

use crate::cli::Options;
use crate::home::Home;
use crate::store::{ChatState, State};
use rovik_core::agent_run::autopilot;
use rovik_core::chat_db::store;
use rovik_protocol::autopilot::ChatTask;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

/// The chats, each with its latest task, oldest chat id first.
pub fn chat_tasks<'a>(
    conn: Option<&Connection>,
    chats: impl IntoIterator<Item = (&'a String, &'a ChatState)>,
) -> Vec<ChatTask> {
    chats
        .into_iter()
        .map(|(id, chat)| {
            let title = conn
                .and_then(|c| store::get_dialog(c, id).ok().flatten())
                .map(|d| d.title)
                .unwrap_or_default();
            let rows = conn
                .and_then(|c| store::messages_with_meta(c, id, "stage", None).ok())
                .unwrap_or_default();
            ChatTask {
                chat: id.clone(),
                title,
                computer: chat.computer.clone(),
                running: chat.running.then(|| chat.run_id.clone()).flatten(),
                outcome: chat.outcome.clone(),
                task: autopilot::task_state(&rows),
            }
        })
        .collect()
}

/// The chats of the daemon's folder `home`, as `status` lists them.
pub fn of_home(home: &Home) -> Result<Vec<ChatTask>, String> {
    let file = home.root.join("state.json");
    let state: State = match std::fs::read(&file) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", file.display()))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    let conn = open_read_only(&home.data().join("chats.db"));
    Ok(chat_tasks(conn.as_ref(), state.chats.iter()))
}

fn open_read_only(path: &Path) -> Option<Connection> {
    if !path.is_file() {
        return None;
    }
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
}

pub fn status(options: Options) -> Result<(), String> {
    let home = Home::locate(options.data)?;
    let chats = of_home(&home)?;
    if options.json {
        let report = serde_json::json!({ "chats": chats });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    if chats.is_empty() {
        println!("No chats here.");
    }
    for chat in &chats {
        println!("{}", line(chat));
    }
    Ok(())
}

/// A chat in a line: its title, its task's stage and how it is, what it
/// spent, and whether a run goes.
fn line(chat: &ChatTask) -> String {
    let name = if chat.title.is_empty() {
        chat.chat.as_str()
    } else {
        chat.title.as_str()
    };
    let mut line = format!("{name} ({}, from {})", chat.chat, chat.computer);
    match &chat.task {
        Some(task) => {
            line.push_str(&format!(": {} {}", task.stage, task.status));
            if let Some(outcome) = &task.outcome {
                line.push_str(&format!(", {outcome}"));
            }
            let time = rovik_core::agent_run::report::duration(task.ms);
            line.push_str(&format!("; {time}, {:.2}", task.cost));
        }
        None => {
            if let Some(outcome) = &chat.outcome {
                line.push_str(&format!(": last run {outcome}"));
            }
        }
    }
    if chat.running.is_some() {
        line.push_str("; running");
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use rovik_protocol::autopilot::TaskState;

    #[test]
    fn a_chat_in_a_line() {
        let chat = ChatTask {
            chat: "c1".into(),
            title: "Make the test pass".into(),
            computer: "laptop".into(),
            running: Some("run-2".into()),
            outcome: None,
            task: Some(TaskState {
                id: "u1".into(),
                stage: "implement".into(),
                status: "running".into(),
                ms: 125_000,
                cost: 0.4,
                ..Default::default()
            }),
        };
        assert_eq!(
            line(&chat),
            "Make the test pass (c1, from laptop): implement running; 2 min, 0.40; running"
        );
        let plain = ChatTask {
            chat: "c2".into(),
            computer: "desk".into(),
            outcome: Some("done".into()),
            ..Default::default()
        };
        assert_eq!(line(&plain), "c2 (c2, from desk): last run done");
    }
}
