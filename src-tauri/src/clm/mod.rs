//! Context Language Models (Shao et al. 2026): the agent manages its own
//! context by editing it as a file.
//!
//! A chat's context mode is "trimming" (chat_db/history.rs trims and
//! shortens the history to fit) or "clm". In CLM mode, before every request:
//!
//! 1. The context file on the virtual volume (`context/<chat>/LIVE_CONTEXT.md`,
//!    `src/vfs`) is read back. If the agent changed it since it was written,
//!    the edit is parsed (document.rs) and, if it fits the budget, becomes
//!    the chat's context: the `projection`, covering stored messages up to
//!    the one the file was written after. Rejected edits are reported.
//! 2. The context sent is the projection plus every message stored since
//!    (each once).
//!    Stored messages are never changed: the projection is a view.
//! 3. If that is over budget, the oldest tool outputs since the projection
//!    are withheld; still over, this request is trimmed as in Trimming mode.
//! 4. The file is rewritten with that context, so the agent always edits
//!    what it sees. Every state of the file is a revision (vfs/history.rs);
//!    revisions are also saved in the database so they survive a restart.
//! 5. Notes for the model go at the end of the request: the edit's receipt,
//!    the size readout and budget reminders (prompt.rs).
//!
//! The agent edits the file with its ordinary tools (read_file, edit_file,
//! write_file through `@context/…`, or run_command through its real path):
//! the paper's "context as a file", no dedicated context tools. When it
//! repeats a call whose earlier result it removed, `clm_recall_hint` points
//! it at the revision that still holds it.

pub mod document;
pub mod prompt;
pub mod store;

use crate::chat_db::history::{fit_with, load_bodies, load_messages, total, Cost, FitInfo, Msg};
use crate::chat_db::DbState;
use crate::vfs::{Source, VfsState};
use document::CtxMsg;
use serde::Serialize;
use tauri::State;

pub const MODE_TRIMMING: &str = "trimming";
pub const MODE_CLM: &str = "clm";

/// Revisions put back on the volume after a restart.
const REFILL_REVISIONS: usize = 64;
/// Tool outputs shorter than this aren't worth withholding.
const WITHHOLD_MIN_CHARS: usize = 400;

/// The chat's folder on the volume.
pub fn context_dir(dialog_id: &str) -> String {
    let safe: String = dialog_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    format!("context/{safe}")
}

pub fn context_file(dialog_id: &str) -> String {
    format!("{}/LIVE_CONTEXT.md", context_dir(dialog_id))
}

/// Where the volume is mounted, or why CLM mode can't be used.
pub fn availability(vfs: &VfsState) -> Result<std::path::PathBuf, String> {
    if let Some(at) = vfs.mountpoint() {
        return Ok(at);
    }
    Err(match vfs.problem() {
        Some(p) => p.message(),
        None => "Virtual files aren't mounted.".to_string(),
    })
}

fn real_path(vfs: &VfsState, rel: &str) -> Option<String> {
    vfs.mountpoint().map(|at| {
        rel.split('/')
            .fold(at, |p, part| p.join(part))
            .to_string_lossy()
            .into_owned()
    })
}

/// The context file's real path and folder, for the agent's shell.
pub fn real_paths(vfs: &VfsState, dialog_id: &str) -> Option<(String, String)> {
    Some((
        real_path(vfs, &context_file(dialog_id))?,
        real_path(vfs, &context_dir(dialog_id))?,
    ))
}

fn ctx_tokens(messages: &[CtxMsg], cost: Cost) -> usize {
    let msgs: Vec<Msg> = messages.iter().map(CtxMsg::to_msg).collect();
    total(&msgs, cost)
}

/// The agent's version of the context followed by the messages stored
/// after it, each message once: a stored message the agent's version
/// already has (as it was, or rewritten) isn't added again. That happens
/// when the file was written but the state saying so wasn't saved, so a
/// newer rendering is read back as an edit.
fn with_later(mut base: Vec<CtxMsg>, later: &[CtxMsg]) -> Vec<CtxMsg> {
    let covered: std::collections::HashSet<String> = base
        .iter()
        .map(|m| {
            m.id.strip_suffix(document::EDITED)
                .unwrap_or(&m.id)
                .to_string()
        })
        .collect();
    base.extend(later.iter().filter(|m| !covered.contains(&m.id)).cloned());
    base
}

/// The document without its metadata line, to tell real changes apart.
fn without_meta(text: &str) -> String {
    text.lines()
        .filter(|l| !l.starts_with(document::META_PREFIX))
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn read_text(vfs: &VfsState, path: &str) -> Option<String> {
    vfs.read_file(path)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// A request's history in CLM mode.
pub struct Prepared {
    pub messages: Vec<Msg>,
    pub info: FitInfo,
    /// Notes for the model, added to the end of the request.
    pub notes: Vec<String>,
}

/// Steps 1–5 above. Fails only for database or volume errors.
pub async fn prepare(
    db: &DbState,
    vfs: &VfsState,
    dialog_id: String,
    budget: usize,
    cost: Cost,
    current_user_content: Option<String>,
) -> Result<Prepared, String> {
    let path = context_file(&dialog_id);
    let on_volume = read_text(vfs, &path);

    let (raw, mut state, saved_revisions, db_max) = {
        let d = dialog_id.clone();
        let refill = on_volume.is_none();
        db.read(move |conn| {
            let mut raw = load_messages(conn, &d, current_user_content.as_deref())?;
            load_bodies(conn, &mut raw)?;
            let state = store::load_state(conn, &d)?;
            let saved = if refill {
                store::latest_revisions(conn, &d, REFILL_REVISIONS)?
            } else {
                Vec::new()
            };
            let max = store::max_revision(conn, &d)?;
            Ok((raw, state, saved, max))
        })
        .await?
    };
    let raw: Vec<CtxMsg> = raw.iter().map(CtxMsg::from_msg).collect();
    let index_of = |id: &str| raw.iter().position(|m| m.id == id);
    let mut notes = Vec::new();

    // After a restart the volume is empty: put the file and its history back.
    let on_volume = match (on_volume, &state.rendered) {
        (Some(text), _) => Some(text),
        (None, Some(rendered)) => {
            vfs.write_unmounted(&path, rendered.as_bytes())?;
            if !saved_revisions.is_empty() {
                let revisions = saved_revisions
                    .into_iter()
                    .map(|(row, content)| {
                        let source = if row.kind == "context" {
                            Source::App
                        } else {
                            Source::Process
                        };
                        (
                            row.number,
                            content.into_bytes(),
                            row.created_at.max(0) as u64,
                            source,
                        )
                    })
                    .collect();
                vfs.import_history(&path, revisions)?;
            }
            Some(rendered.clone())
        }
        (None, None) => None,
    };

    // 1. The agent's edit, if any.
    if let (Some(text), Some(rendered), Some(rendered_messages)) =
        (&on_volume, &state.rendered, &state.rendered_messages)
    {
        if without_meta(text) != without_meta(rendered) {
            match state.rendered_anchor_id.as_deref().and_then(index_of) {
                None => {
                    notes.push(
                        "[context: your edit to the context file was not applied: the chat's stored history \
                         changed since the file was written (messages were retried or removed). The context \
                         was rebuilt from the chat.]"
                            .to_string(),
                    );
                    state.projection = None;
                    state.anchor_id = None;
                }
                Some(at) => {
                    let (candidate, summary) = document::parse_back(text, rendered_messages, &raw);
                    let whole = with_later(candidate.clone(), &raw[at + 1..]);
                    let before = ctx_tokens(rendered_messages, cost);
                    let after = ctx_tokens(&candidate, cost);
                    let latest = vfs
                        .revisions(&path)
                        .ok()
                        .and_then(|r| r.last().map(|r| r.number));
                    let backup = state.rendered_revision;
                    let backup_note = backup
                        .map(|b| {
                            format!(
                                " The context before it is revision {b} ({}/{b}.md).",
                                prompt::REVISIONS_PATH
                            )
                        })
                        .unwrap_or_default();
                    if !whole.iter().any(|m| m.role == "user") {
                        notes.push(format!(
                            "[context: your edit was REJECTED, not applied: it would leave your context with no \
                             messages. The file was rewritten with your context as it was; your edited version is \
                             revision {}. Keep at least a short note of the task and where you are.]",
                            latest.map_or("?".into(), |n| n.to_string())
                        ));
                    } else if after > before && ctx_tokens(&whole, cost) > budget {
                        // The paper's "fit" gate: an edit may grow the context
                        // only while it still fits. One that shrinks it is
                        // always taken, even if the context is still over.
                        notes.push(format!(
                            "[context: your edit was REJECTED, not applied: the context would be ~{after} tokens, \
                             over the ~{budget}-token budget. The file was rewritten with your context as it was \
                             (~{before} tokens); your edited version is revision {}. Replace stale text with \
                             SHORTER summaries instead of adding to it.]",
                            latest.map_or("?".into(), |n| n.to_string())
                        ));
                    } else {
                        let grew = if after > before {
                            " It GREW the context; if you meant to condense, you likely duplicated text instead of replacing it."
                        } else {
                            ""
                        };
                        let flattened = if summary.flattened > 0 {
                            format!(
                                " {} tool call(s) whose results you changed are now plain text.",
                                summary.flattened
                            )
                        } else {
                            String::new()
                        };
                        let still_over = if ctx_tokens(&whole, cost) > budget {
                            format!(
                                " The context is STILL OVER the ~{budget}-token budget: compact more now (delete stale \
                                 outputs and finished work)."
                            )
                        } else {
                            String::new()
                        };
                        notes.push(format!(
                            "[context: edit applied as revision {}: ~{before} → ~{after} tokens; blocks: {}.{flattened}{grew}{still_over}{backup_note}]",
                            latest.map_or("?".into(), |n| n.to_string()),
                            summary.describe()
                        ));
                        state.projection = Some(candidate);
                        state.anchor_id = state.rendered_anchor_id.clone();
                        state.nudged.clear();
                    }
                }
            }
        }
    }

    // 2. The context: the agent's version plus what came since.
    let (mut effective, projected) = match (
        &state.projection,
        state.anchor_id.as_deref().map(index_of),
    ) {
        (Some(p), Some(Some(at))) => (with_later(p.clone(), &raw[at + 1..]), p.len()),
        (Some(_), _) => {
            notes.push(
                "[context: the chat's stored history changed (messages were retried or removed), so your \
                 edited context no longer applies; it was rebuilt from the chat. Earlier revisions still hold \
                 your version.]"
                    .to_string(),
            );
            state.projection = None;
            state.anchor_id = None;
            (raw.clone(), 0)
        }
        _ => (raw.clone(), 0),
    };
    let (repaired, _) = document::repair(effective, &raw);
    effective = repaired;
    let full_tokens = ctx_tokens(&raw, cost);

    // 3. Over budget: withhold the oldest outputs since the projection.
    let mut tokens = ctx_tokens(&effective, cost);
    let mut withheld = 0;
    if tokens > budget {
        let last = effective.len().saturating_sub(1);
        for i in projected.min(last)..last {
            let m = &mut effective[i];
            if m.role != "tool" || m.text.chars().count() < WITHHOLD_MIN_CHARS {
                continue;
            }
            let size = cost.tokens(m.text.chars().count());
            m.text = format!(
                "[Output withheld to fit your context budget (~{size} tokens). Compact your context file \
                 to make room, then run the tool again with narrower output if you still need it.]"
            );
            m.attachments.clear();
            withheld += 1;
            tokens = ctx_tokens(&effective, cost);
            if tokens <= budget {
                break;
            }
        }
        if withheld > 0 {
            notes.push(format!(
                "[context: over budget, so {withheld} older tool output(s) since your last edit were withheld. \
                 Compact your context file now.]"
            ));
        }
    }

    // 4. Rewrite the file with exactly that context.
    let before_render = vfs
        .revisions(&path)
        .ok()
        .and_then(|r| r.last().map(|r| r.number));
    let rendered = document::render(&effective, before_render.map_or(1, |n| n + 1));
    let current = read_text(vfs, &path);
    let file_text = match current {
        Some(c) if without_meta(&c) == without_meta(&rendered) => c,
        _ => {
            vfs.write_unmounted(&path, rendered.as_bytes())?;
            rendered
        }
    };
    let volume_revisions = vfs.revisions(&path).unwrap_or_default();
    let rendered_revision = volume_revisions.last().map(|r| r.number);

    // Revisions the database doesn't have yet.
    let previous_render = state.rendered_revision;
    let new_revisions: Vec<store::NewRevision> = volume_revisions
        .iter()
        .filter(|r| db_max.is_none_or(|m| r.number > m))
        .filter_map(|r| {
            let content =
                String::from_utf8_lossy(&vfs.read_revision(&path, r.number).ok()?).into_owned();
            let kind = if Some(r.number) == rendered_revision || Some(r.number) == previous_render {
                "context"
            } else {
                "edit"
            };
            Some(store::NewRevision {
                number: r.number,
                kind: kind.into(),
                created_at: r.time as i64,
                tokens: cost.tokens(content.chars().count()),
                content,
            })
        })
        .collect();

    // 5. Notes for the model.
    notes.push(prompt::readout(tokens, budget, rendered_revision));
    if let Some(nudge) = prompt::nudge(tokens, budget, &mut state.nudged) {
        notes.push(nudge);
    }

    // Last resort: still over, this request is trimmed as in Trimming mode.
    let msgs: Vec<Msg> = effective.iter().map(CtxMsg::to_msg).collect();
    let (messages, info) = if tokens > budget {
        notes.push(
            "[context: still over budget after withholding outputs, so older parts were cut from this request. \
             Compact your context file NOW.]"
                .to_string(),
        );
        let (fitted, mut info) = fit_with(msgs, budget, cost);
        info.full_tokens = full_tokens;
        (fitted, info)
    } else {
        (
            msgs,
            FitInfo {
                trimmed: withheld > 0,
                dropped_turns: 0,
                estimated_tokens: tokens,
                full_tokens,
                outdated: 0,
            },
        )
    };

    state.tokens = tokens;
    state.budget = budget;
    state.rendered = Some(file_text);
    state.rendered_messages = Some(effective);
    state.rendered_anchor_id = raw.last().map(|m| m.id.clone());
    state.rendered_revision = rendered_revision;
    db.write(move |conn| {
        let tx = conn.transaction().map_err(crate::chat_db::store::err)?;
        store::save_state(&tx, &dialog_id, &state)?;
        store::insert_revisions(&tx, &dialog_id, &new_revisions)?;
        tx.commit().map_err(crate::chat_db::store::err)
    })
    .await?;

    Ok(Prepared {
        messages,
        info,
        notes,
    })
}

/// The system prompt section for a chat in CLM mode.
pub async fn system_section(
    db: &DbState,
    vfs: &VfsState,
    dialog_id: &str,
    budget: usize,
) -> Option<String> {
    let (real_file, real_dir) = real_paths(vfs, dialog_id)?;
    let d = dialog_id.to_string();
    let state = db
        .read(move |conn| store::load_state(conn, &d))
        .await
        .ok()?;
    Some(prompt::system_section(
        &real_file,
        &real_dir,
        budget,
        state.rendered_revision,
    ))
}

/*
 * ---------------------------------------------------------------------------
 * The agent's file tools on `@context/…`
 * ---------------------------------------------------------------------------
 */

enum Target {
    Live,
    Revisions,
    Revision(u64, Option<usize>),
    /// Another file in the chat's folder (the agent's own notes).
    Other(String),
}

/// What a path the agent used means: `@context/…`, or the folder's real path.
fn target(vfs: &VfsState, dialog_id: &str, path: &str) -> Result<Target, String> {
    let path = path.trim().replace('\\', "/");
    let rel = if let Some(rest) = path.strip_prefix("@context") {
        rest.trim_start_matches('/').to_string()
    } else if let Some(dir) = real_paths(vfs, dialog_id).map(|(_, d)| d.replace('\\', "/")) {
        match path.strip_prefix(&dir) {
            Some(rest) => rest.trim_start_matches('/').to_string(),
            None => return Err(format!("{path} is not in your context folder")),
        }
    } else {
        return Err(format!("{path} is not in your context folder"));
    };
    if rel.is_empty() || rel == "LIVE_CONTEXT.md" {
        return Ok(Target::Live);
    }
    if rel == "revisions" || rel == "revisions/" {
        return Ok(Target::Revisions);
    }
    if let Some(name) = rel.strip_prefix("revisions/") {
        let (file, turn) = match name.split_once('#') {
            Some((f, t)) => (
                f,
                Some(
                    t.trim()
                        .trim_start_matches("turn")
                        .trim_start_matches(['-', '='])
                        .parse::<usize>()
                        .map_err(|_| format!("bad block number in {path}"))?,
                ),
            ),
            None => (name, None),
        };
        let number = file
            .trim_end_matches(".md")
            .trim_start_matches('r')
            .parse::<u64>()
            .map_err(|_| {
                format!(
                    "{path}: no such revision; read {} for the list",
                    prompt::REVISIONS_PATH
                )
            })?;
        return Ok(Target::Revision(number, turn));
    }
    if rel.split('/').any(|p| p == ".." || p.is_empty()) {
        return Err(format!("{path}: invalid path"));
    }
    Ok(Target::Other(rel))
}

fn line_range(content: &str, start: Option<usize>, end: Option<usize>) -> String {
    if start.is_none() && end.is_none() {
        const MAX_CHARS: usize = 20_000;
        let count = content.chars().count();
        if count > MAX_CHARS {
            let head: String = content.chars().take(MAX_CHARS).collect();
            return format!(
                "{head}\n\n[...truncated, {} more characters — request a line range to see more]",
                count - MAX_CHARS
            );
        }
        return content.to_string();
    }
    let lines: Vec<&str> = content.lines().collect();
    let start = start.unwrap_or(1).max(1);
    let end = end.unwrap_or(lines.len()).min(lines.len());
    if start > end {
        return String::new();
    }
    lines[start - 1..end].join("\n")
}

async fn revision_text(
    db: &DbState,
    vfs: &VfsState,
    dialog_id: &str,
    number: u64,
) -> Result<String, String> {
    if let Ok(data) = vfs.read_revision(&context_file(dialog_id), number) {
        return Ok(String::from_utf8_lossy(&data).into_owned());
    }
    let d = dialog_id.to_string();
    db.read(move |conn| store::revision_content(conn, &d, number))
        .await?
        .ok_or_else(|| {
            format!(
                "revision {number} isn't kept; read {} for the list",
                prompt::REVISIONS_PATH
            )
        })
}

fn ago(ms: i64) -> String {
    let secs = ((store::now_ms() - ms) / 1000).max(0);
    match secs {
        0..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

async fn revisions_listing(db: &DbState, dialog_id: &str) -> Result<String, String> {
    let d = dialog_id.to_string();
    let rows = db.read(move |conn| store::revisions(conn, &d)).await?;
    if rows.is_empty() {
        return Ok("No revisions yet: the context file is written before the next request.".into());
    }
    let mut out = vec!["Revisions of your context file, oldest first (older plain snapshots may have been dropped):".to_string()];
    for r in &rows {
        let what = match r.kind.as_str() {
            "context" => "the context as sent",
            "edit" => "edited (by you)",
            "restore" => "restored by the user",
            other => other,
        };
        out.push(format!(
            "  {:>4}  ~{} tokens  {what}, {}",
            r.number,
            r.tokens,
            ago(r.created_at)
        ));
    }
    out.push(format!(
        "Read one with read_file {}/<number>.md (start_line/end_line), or {}/<number>.md#<block> for one block.",
        prompt::REVISIONS_PATH,
        prompt::REVISIONS_PATH
    ));
    Ok(out.join("\n"))
}

#[tauri::command]
pub async fn clm_tool_read(
    db: State<'_, DbState>,
    vfs: State<'_, VfsState>,
    dialog_id: String,
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<String, String> {
    tool_read(&db, &vfs, &dialog_id, &path, start_line, end_line).await
}

/// read_file on `@context/…`.
pub async fn tool_read(
    db: &DbState,
    vfs: &VfsState,
    dialog_id: &str,
    path: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<String, String> {
    match target(vfs, dialog_id, path)? {
        Target::Live => {
            let text = read_text(vfs, &context_file(dialog_id)).ok_or(
                "Your context file is written before the next request; it doesn't exist yet.",
            )?;
            let note = if start_line.is_none() && end_line.is_none() {
                "\n\n[Reminder: this file's text is already your context; don't read it whole again. Edit it in place.]"
            } else {
                ""
            };
            Ok(format!("{}{note}", line_range(&text, start_line, end_line)))
        }
        Target::Revisions => revisions_listing(db, dialog_id).await,
        Target::Revision(number, turn) => {
            let text = revision_text(db, vfs, dialog_id, number).await?;
            match turn {
                Some(t) => document::turn_text(&text, t)
                    .ok_or_else(|| format!("revision {number} has no block {t}")),
                None => Ok(line_range(&text, start_line, end_line)),
            }
        }
        Target::Other(rel) => {
            let p = format!("{}/{rel}", context_dir(dialog_id));
            let text = read_text(vfs, &p).ok_or_else(|| format!("{path}: not found"))?;
            Ok(line_range(&text, start_line, end_line))
        }
    }
}

const APPLIES: &str = "It becomes your context when your next request is sent.";

#[tauri::command]
pub async fn clm_tool_edit(
    vfs: State<'_, VfsState>,
    dialog_id: String,
    path: String,
    old_text: String,
    new_text: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<String, String> {
    tool_edit(
        &vfs, &dialog_id, &path, &old_text, &new_text, start_line, end_line,
    )
}

/// The file a write to `path` changes: the context file or one of the
/// agent's own files, never a revision.
fn writable(vfs: &VfsState, dialog_id: &str, path: &str) -> Result<String, String> {
    match target(vfs, dialog_id, path)? {
        Target::Live => Ok(context_file(dialog_id)),
        Target::Other(rel) => Ok(format!("{}/{rel}", context_dir(dialog_id))),
        _ => Err(format!(
            "Revisions can't be changed. Edit {} (copy what you need from a revision into it).",
            prompt::CONTEXT_PATH
        )),
    }
}

/// edit_file on `@context/…`.
pub fn tool_edit(
    vfs: &VfsState,
    dialog_id: &str,
    path: &str,
    old_text: &str,
    new_text: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<String, String> {
    let file = writable(vfs, dialog_id, path)?;
    let content = read_text(vfs, &file).ok_or_else(|| format!("{path}: not found"))?;
    let (updated, span) =
        crate::edit_file::apply_edit(&content, old_text, new_text, start_line, end_line)?;
    vfs.write_unmounted(&file, updated.as_bytes())?;
    let what = crate::edit_file::describe_edit(path, &span);
    Ok(if file == context_file(dialog_id) {
        format!("{what} {APPLIES}")
    } else {
        what
    })
}

#[tauri::command]
pub async fn clm_tool_write(
    vfs: State<'_, VfsState>,
    dialog_id: String,
    path: String,
    content: String,
) -> Result<String, String> {
    tool_write(&vfs, &dialog_id, &path, &content)
}

/// write_file on `@context/…`.
pub fn tool_write(
    vfs: &VfsState,
    dialog_id: &str,
    path: &str,
    content: &str,
) -> Result<String, String> {
    let file = writable(vfs, dialog_id, path)?;
    vfs.write_unmounted(&file, content.as_bytes())?;
    let lines = content.lines().count();
    Ok(if file == context_file(dialog_id) {
        format!("Wrote {path} ({lines} lines). {APPLIES}")
    } else {
        format!("Wrote {path} ({lines} lines).")
    })
}

/// Same tool, same target: whether `a` repeats `b`.
fn same_call(name: &str, a: &str, b: &str) -> bool {
    let parse =
        |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap_or(serde_json::Value::Null);
    let (a, b) = (parse(a), parse(b));
    if a == b {
        return true;
    }
    match name {
        // Another range of the same file, or the same folder deeper.
        "read_file" | "list_dir" => a.get("path").is_some() && a.get("path") == b.get("path"),
        "search_code" => a.get("query").is_some() && a.get("query") == b.get("query"),
        "run_command" => a.get("command").is_some() && a.get("command") == b.get("command"),
        _ => false,
    }
}

/// When the agent repeats a call whose earlier result it removed from its
/// context, a note offering the revision that still holds that result.
#[tauri::command]
pub async fn clm_recall_hint(
    db: State<'_, DbState>,
    dialog_id: String,
    tool_call_id: String,
    name: String,
    arguments: String,
) -> Result<Option<String>, String> {
    db.read(move |conn| recall_hint(conn, &dialog_id, &tool_call_id, &name, &arguments))
        .await
}

pub fn recall_hint(
    conn: &rusqlite::Connection,
    dialog_id: &str,
    tool_call_id: &str,
    name: &str,
    arguments: &str,
) -> Result<Option<String>, String> {
    let state = store::load_state(conn, dialog_id)?;
    let Some(projection) = state.projection else {
        return Ok(None);
    };
    let raw = load_messages(conn, dialog_id, None)?;
    let Some(at) = state
        .anchor_id
        .as_deref()
        .and_then(|a| raw.iter().position(|m| m.id == a))
    else {
        return Ok(None);
    };
    // Results the agent still sees as they were.
    let visible: std::collections::HashSet<&str> = projection
        .iter()
        .filter(|m| m.role == "tool")
        .map(|m| m.id.as_str())
        .chain(raw[at + 1..].iter().map(|m| m.id.as_str()))
        .collect();
    let earlier = raw
        .iter()
        .rev()
        .filter(|m| m.role == "assistant")
        .flat_map(|m| m.tool_calls.iter().flatten())
        .find(|c| {
            c.id != tool_call_id && c.name == name && same_call(name, arguments, &c.arguments)
        });
    let Some(call) = earlier else { return Ok(None) };
    let Some(result) = raw
        .iter()
        .find(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some(call.id.as_str()))
    else {
        return Ok(None);
    };
    if visible.contains(result.id.as_str()) {
        return Ok(None);
    }
    // The newest revision with the result as it was (the agent may have
    // kept the header and rewritten the text).
    let mut result = result.clone();
    load_bodies(conn, std::slice::from_mut(&mut result))?;
    let original = document::body(&CtxMsg::from_msg(&result));
    let start: String = original.chars().take(200).collect();
    let found = store::revisions_with_message(conn, dialog_id, &result.id, 50)?
        .into_iter()
        .find_map(|(n, content)| {
            let turn = document::turn_of(&content, &result.id)?;
            let text = document::turn_text(&content, turn)?;
            let body = text.split_once('\n').map_or("", |(_, b)| b);
            body.trim_start()
                .starts_with(start.trim())
                .then_some((n, turn))
        });
    let Some((number, turn)) = found else {
        return Ok(None);
    };
    let block = format!("#{turn}");
    Ok(Some(format!(
        "\n\n[context: you ran this before, and that earlier result is no longer in your context (you removed \
         or rewrote it). Do you need the earlier result? Revision {number} still holds it: read_file {}/{number}.md{block}. \
         If things may have changed since, rely on the fresh result above.]",
        prompt::REVISIONS_PATH
    )))
}

/*
 * ---------------------------------------------------------------------------
 * The chat's context mode and CLM state, for the UI
 * ---------------------------------------------------------------------------
 */

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ClmStatus {
    /// The chat's mode; None until one is chosen for it.
    pub mode: Option<String>,
    /// Virtual files are mounted, so CLM mode can run.
    pub available: bool,
    /// Why not, for the user.
    pub problem: Option<String>,
    pub path: String,
    pub real_path: Option<String>,
    /// The file's latest revision written for a request.
    pub revision: Option<u64>,
    pub revisions: usize,
    pub edits: usize,
    /// The agent's own context is in use (it has edited it).
    pub projected: bool,
    pub tokens: usize,
    pub budget: usize,
}

#[tauri::command]
pub async fn clm_status(
    db: State<'_, DbState>,
    vfs: State<'_, VfsState>,
    dialog_id: String,
) -> Result<ClmStatus, String> {
    status(&db, &vfs, dialog_id).await
}

/// The chat's CLM state as it is now (also sent by a run after each request
/// is prepared, so the UI follows it turn by turn).
pub async fn status(db: &DbState, vfs: &VfsState, dialog_id: String) -> Result<ClmStatus, String> {
    let available = availability(vfs);
    let real = real_paths(vfs, &dialog_id).map(|(f, _)| f);
    let d = dialog_id.clone();
    let (mode, state, revisions) = db
        .read(move |conn| {
            Ok((
                store::context_mode(conn, &d)?,
                store::load_state(conn, &d)?,
                store::revisions(conn, &d)?,
            ))
        })
        .await?;
    Ok(ClmStatus {
        mode,
        available: available.is_ok(),
        problem: available.err(),
        path: prompt::CONTEXT_PATH.into(),
        real_path: real,
        revision: state.rendered_revision,
        revisions: revisions.len(),
        edits: revisions.iter().filter(|r| r.kind != "context").count(),
        projected: state.projection.is_some(),
        tokens: state.tokens,
        budget: state.budget,
    })
}

#[tauri::command]
pub async fn clm_set_mode(
    db: State<'_, DbState>,
    dialog_id: String,
    mode: String,
) -> Result<(), String> {
    if mode != MODE_TRIMMING && mode != MODE_CLM {
        return Err(format!("unknown context mode {mode}"));
    }
    db.write(move |conn| store::set_context_mode(conn, &dialog_id, &mode))
        .await
}

/// Drops the agent's context; the chat is sent as stored again.
#[tauri::command]
pub async fn clm_reset(db: State<'_, DbState>, dialog_id: String) -> Result<(), String> {
    db.write(move |conn| store::clear_projection(conn, &dialog_id))
        .await
}

#[tauri::command]
pub async fn clm_list_revisions(
    db: State<'_, DbState>,
    dialog_id: String,
) -> Result<Vec<store::RevisionRow>, String> {
    db.read(move |conn| store::revisions(conn, &dialog_id))
        .await
}

#[tauri::command]
pub async fn clm_read_revision(
    db: State<'_, DbState>,
    vfs: State<'_, VfsState>,
    dialog_id: String,
    number: u64,
) -> Result<String, String> {
    revision_text(&db, &vfs, &dialog_id, number).await
}

/// Puts revision `number` back as the context file: it's read back like an
/// edit before the next request.
#[tauri::command]
pub async fn clm_restore_revision(
    db: State<'_, DbState>,
    vfs: State<'_, VfsState>,
    dialog_id: String,
    number: u64,
) -> Result<(), String> {
    restore_revision(&db, &vfs, &dialog_id, number).await
}

/// An older revision to put back, followed by the blocks of the current
/// context for messages stored after the newest one it has: restoring
/// brings old content back without losing what happened since (the user's
/// latest message, say).
fn restored_text(old: &str, current: &[CtxMsg], stored: &[String]) -> String {
    let position = |id: &str| {
        stored
            .iter()
            .position(|s| s == id.strip_suffix(document::EDITED).unwrap_or(id))
    };
    let newest = document::blocks(old)
        .iter()
        .filter_map(|(h, _)| h.as_ref()?.id.as_deref().and_then(position))
        .max();
    let after: Vec<CtxMsg> = current
        .iter()
        .filter(|m| match (position(&m.id), newest) {
            (Some(at), Some(newest)) => at > newest,
            (Some(_), None) => true,
            (None, _) => false,
        })
        .cloned()
        .collect();
    if after.is_empty() {
        return old.to_string();
    }
    let tail = document::render(&after, 0);
    let blocks = tail.split_once('\n').map_or("", |(_, rest)| rest);
    format!("{}\n{blocks}", old.trim_end())
}

pub async fn restore_revision(
    db: &DbState,
    vfs: &VfsState,
    dialog_id: &str,
    number: u64,
) -> Result<(), String> {
    let old = revision_text(db, vfs, dialog_id, number).await?;
    let d = dialog_id.to_string();
    let (rendered, stored) = db
        .read(move |conn| {
            let state = store::load_state(conn, &d)?;
            let stored: Vec<String> = load_messages(conn, &d, None)?
                .into_iter()
                .map(|m| m.id)
                .collect();
            Ok((state.rendered_messages.unwrap_or_default(), stored))
        })
        .await?;
    let text = restored_text(&old, &rendered, &stored);
    let path = context_file(dialog_id);
    vfs.write_unmounted(&path, text.as_bytes())?;
    let Some(latest) = vfs.revisions(&path).ok().and_then(|r| r.last().cloned()) else {
        return Ok(());
    };
    let row = store::NewRevision {
        number: latest.number,
        kind: "restore".into(),
        created_at: latest.time as i64,
        tokens: Cost {
            vision: false,
            chars_per_token: crate::chat_db::history::DEFAULT_CHARS_PER_TOKEN,
        }
        .tokens(text.chars().count()),
        content: text,
    };
    let d = dialog_id.to_string();
    db.write(move |conn| store::insert_revisions(conn, &d, &[row]))
        .await
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod edge_tests;
