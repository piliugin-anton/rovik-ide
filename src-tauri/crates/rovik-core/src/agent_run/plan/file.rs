//! A plan's file in `.rovik/plans/`: where it's saved, how its path is
//! shown, the request that implements it and the context that carries it
//! into later requests.

use serde_json::Value;
use std::path::Path;

pub const PLANS_DIR: &str = ".rovik/plans";

/// "2026-10-04-add-a-users-table" from the request.
pub(super) fn file_stem(request: &str, date: &str) -> String {
    let lower = request.to_lowercase();
    let words: Vec<String> = lower
        .split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
        .filter(|w| !w.is_empty())
        .take(7)
        .map(str::to_string)
        .collect();
    let slug: String = words.join("-").chars().take(60).collect();
    format!("{date}-{}", if slug.is_empty() { "plan" } else { &slug })
}

/// `root` and `rel` (written with `/`) as one path in the OS's own form: the
/// file tree and editor tabs name files that way, so `C:\p/.rovik/plans`
/// would open as a second tab of the same file on Windows.
fn join(root: &str, rel: &str) -> String {
    let mut path = std::path::PathBuf::from(root);
    path.extend(rel.split('/'));
    path.to_string_lossy().into_owned()
}

/// Whether `path` names a plan's file as `save_plan_file` makes them: a
/// Markdown file in a `.rovik/plans` folder (`PLANS_DIR`), reached through
/// no link. A chat's rows may come from a server (its mirror, a chat moved
/// back), so the file they name is read, sent to a model or written only
/// when it's one: never another of this computer's files.
pub fn is_plan_file(path: &str) -> bool {
    let file = Path::new(path);
    let named = file.parent().is_some_and(|dir| dir.ends_with(PLANS_DIR))
        && file
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("md"));
    if !named {
        return false;
    }
    if std::fs::symlink_metadata(file).is_ok_and(|m| m.file_type().is_symlink()) {
        return false;
    }
    // Where its folder really is: a plans folder too. One not made yet
    // holds nothing to read, and a save makes it.
    match file.parent().map(std::fs::canonicalize) {
        Some(Ok(dir)) => dir.ends_with(PLANS_DIR),
        _ => true,
    }
}

/// Saves a plan and returns its path. `existing`: the chat's earlier plan
/// file, which a revision replaces; otherwise a new name that's free.
pub fn save_plan_file(
    root: &str,
    request: &str,
    plan: &str,
    existing: Option<&str>,
    date: &str,
) -> Result<String, String> {
    let path = match existing {
        Some(p) => p.to_string(),
        None => {
            let stem = file_stem(request, date);
            let mut path = join(root, &format!("{PLANS_DIR}/{stem}.md"));
            let mut n = 2;
            while Path::new(&path).exists() && n < 100 {
                path = join(root, &format!("{PLANS_DIR}/{stem}-{n}.md"));
                n += 1;
            }
            path
        }
    };
    // Round 2: one kind of line ending in the file, as the plan is read.
    let plan = plan.replace("\r\n", "\n");
    let body = plan.trim();
    // A title, if the plan doesn't start with one.
    let titled = if body.starts_with("# ") || body.starts_with("#\t") {
        body.to_string()
    } else {
        let first: String = request
            .split('\n')
            .next()
            .unwrap_or("")
            .chars()
            .take(100)
            .collect();
        format!("# Plan: {first}\n\n{body}")
    };
    crate::file_guard::write_app_file(Path::new(&path), format!("{titled}\n").as_bytes())?;
    Ok(path)
}

/// The plan's path for display: relative to the project.
pub fn relative_plan_path(path: &str, root: Option<&str>) -> String {
    let Some(root) = root else {
        return path.to_string();
    };
    let r = root.trim_end_matches(['/', '\\']);
    match path.strip_prefix(r) {
        // P4.9: only at a separator (`/a/bc` isn't under `/a/b`).
        Some(rest) => match rest.strip_prefix(['/', '\\']) {
            Some(r) if !r.is_empty() => r.to_string(),
            _ => path.to_string(),
        },
        None => path.to_string(),
    }
}

/// The text that implements a plan (Agent mode): the plan in full, kept in
/// view for the whole run (the current request is never dropped from the
/// context), then how to carry it out. `file`: where it's saved, as the
/// chat shows it. `todo`: the model keeps its checklist with Rovik's `todo`;
/// one without it (a connector's harness with its own tools) uses its own
/// checklist tool, if it has one.
pub fn implementation_request(plan: &str, file: Option<&str>, todo: bool) -> String {
    let checklist = match todo {
        true => concat!(
            "First create your checklist with the todo tool from the plan's steps, with the first step in_progress, then work through them in order, ",
            "one step at a time, each with its own todo call: call todo alone to mark a step in_progress before starting it, ",
            "do the work, then call todo alone again to mark it done before the next step. Never save up finished steps for one update at the end. ",
        ),
        false => concat!(
            "First make a checklist of the plan's steps with your own checklist tool, if you have one, then work through them in order, ",
            "one step at a time: mark a step in progress before starting it, and done before the next. ",
        ),
    };
    // How to carry it out, with either checklist.
    let work = concat!(
        "For each step, read the files it names before changing them, and make ",
        "the changes it describes to the functions, types and settings it names. Follow the plan and its decisions. ",
        "If a step turns out wrong or unnecessary (a file or function isn\u{2019}t where the plan says, or the change ",
        "doesn\u{2019}t fit the code), mark it skipped or changed with the reason instead of silently doing something else. ",
        "When the steps are done, run the checks in the plan\u{2019}s Validation section that you can run (build, ",
        "typecheck, tests, lint), add or update the tests it asks for, and fix what fails. In your final message, ",
        "say which checks passed, which you couldn\u{2019}t run and why, what is left to check by hand, and why you changed ",
        "any files the plan doesn\u{2019}t mention.",
    );
    [
        match file {
            Some(file) => format!(
                "Implement this plan. It's saved in {file}; the user may have edited it, and this is its current version."
            ),
            None => "Implement this plan.".to_string(),
        },
        String::new(),
        "<plan>".into(),
        plan.trim().to_string(),
        "</plan>".into(),
        String::new(),
        format!("{checklist}{work}"),
    ]
    .join("\n")
}

/// "Implement this plan" as the app sends it: `meta.implementsPlan` with
/// the plan's path (`meta.planPath`). The text the model gets, the plan as
/// its file is now, goes in `meta.sentContent` before the message is
/// stored, so every turn and Retry sends it the same. A message that has
/// its text already, or isn't one, is left as it is. Err: the file can't
/// be read. `todo`: the model has Rovik's (`implementation_request`).
pub fn expand_implementation(
    message: &mut super::super::UserMessage,
    root: Option<&str>,
    todo: bool,
) -> Result<(), String> {
    let Some(meta) = message.meta.as_mut().and_then(Value::as_object_mut) else {
        return Ok(());
    };
    let implements = meta.get("implementsPlan").and_then(Value::as_bool) == Some(true);
    let path = meta
        .get("planPath")
        .and_then(Value::as_str)
        .map(str::to_string);
    let (Some(path), true, false) = (path, implements, meta.contains_key("sentContent")) else {
        return Ok(());
    };
    let file = match root {
        Some(root) if !Path::new(&path).is_absolute() => join(root, &path),
        _ => path,
    };
    if !is_plan_file(&file) {
        return Err(format!(
            "{file} isn't a plan's file: plans are saved in {PLANS_DIR}."
        ));
    }
    let text = std::fs::read_to_string(&file)
        .map_err(|e| format!("The plan in {file} can't be read: {e}"))?;
    let shown = relative_plan_path(&file, root);
    meta.insert(
        "sentContent".into(),
        Value::String(implementation_request(&text, Some(&shown), todo)),
    );
    Ok(())
}

/// Plan mode: the current plan (as the file is now), for the model to revise.
pub fn current_plan_context(plan_file: Option<&str>, root: Option<&str>) -> String {
    let Some(file) = plan_file else {
        return String::new();
    };
    let Ok(text) = std::fs::read_to_string(file) else {
        return String::new();
    };
    if text.trim().is_empty() {
        return String::new();
    }
    [
        "## The current plan".to_string(),
        format!(
            "Saved in {}; the user may have edited it. This is the version to revise.",
            relative_plan_path(file, root)
        ),
        String::new(),
        "<current_plan>".into(),
        text.trim().to_string(),
        "</current_plan>".into(),
    ]
    .join("\n")
}
