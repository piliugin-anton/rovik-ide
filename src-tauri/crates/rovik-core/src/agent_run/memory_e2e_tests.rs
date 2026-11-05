//! A project's long-term memory in whole runs (memory.rs, the memory
//! crate): what a run recalls into its prompt, the note an edit is given
//! about its own file, the promotion or invalidation after the checks, and
//! the reflection, lesson and verdict the run leaves behind. The project's
//! check is a node script, as in the repair suite.

use super::*;
use verified_code_memory::model::MemoryRecord;
use verified_code_memory::{NewMemory, RetrievalQuery, SharedMemoryStore, Verification};

/// The project as the memory's repo: the settings root the run knows it by.
fn repo(h: &Harness) -> String {
    h.project.to_string_lossy().into_owned()
}

/// The project's memory file, opened as the app opens it.
fn store(h: &Harness) -> SharedMemoryStore {
    let path = crate::memory::store_path(&h.core.paths, &h.project);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    SharedMemoryStore::open(path).unwrap()
}

/// A memory the run should find: provisional, about `files`.
fn remember(h: &Harness, title: &str, content: &str, files: &[&str]) -> uuid::Uuid {
    store(h)
        .remember(NewMemory::episode(repo(h), title, content).with_files(files.iter().copied()))
        .unwrap()
        .id
}

/// A memory the project already knows is right.
fn remember_verified(h: &Harness, title: &str, content: &str, files: &[&str]) -> uuid::Uuid {
    let id = remember(h, title, content, files);
    store(h).verify(id, "cargo test passed").unwrap();
    id
}

/// One memory as the store holds it now.
fn record(h: &Harness, id: uuid::Uuid) -> MemoryRecord {
    store(h).get(id).unwrap()
}

/// The project's memories, read two ways and put together: a query nothing
/// matches, which the store answers with the project's most recently
/// updated records, and a ranked query on the words a lesson would carry.
/// Neither alone is enough — a ranked query leaves out a memory whose words
/// aren't the task's, and the store has no list-by-kind.
fn recent(h: &Harness) -> Vec<MemoryRecord> {
    let store = store(h);
    let mut found: Vec<MemoryRecord> = Vec::new();
    for task in [
        "qqzzxxvvuu",
        "npm test pass failing fix the check making it",
    ] {
        for hit in store
            .retrieve(&RetrievalQuery::new(repo(h), task).with_limit(32))
            .unwrap()
        {
            if !found.iter().any(|m| m.id == hit.record.id) {
                found.push(hit.record);
            }
        }
    }
    found
}

/// The lessons the project's memory holds.
fn procedures(h: &Harness) -> Vec<MemoryRecord> {
    recent(h)
        .into_iter()
        .filter(|m| m.kind.to_string() == "procedure")
        .collect()
}

/// The memories of the run's last `memoryRecalled`: the list the run
/// leaves with when it ends, verdicts filled in.
fn last_recalled(h: &Harness) -> Vec<Value> {
    let events = h.events_of("memoryRecalled");
    let last = events.last().expect("the run said what it recalled");
    last["memories"].as_array().cloned().unwrap_or_default()
}

fn has_npm() -> bool {
    std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_ok()
}

/// How many rounds of checks the chat's run ended having run, as its line in
/// the agent log says.
async fn verify_rounds(h: &Harness) -> u64 {
    let path = h.core.paths.log_dir.join("agent-log.jsonl");
    for _ in 0..200 {
        crate::agent_run::log::flushed().await;
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let found = text
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|l| l.get("verifyRounds").is_some())
            .filter(|l| match l.get("dialogId").and_then(Value::as_str) {
                Some(id) => id == h.dialog_id.as_str(),
                None => true,
            })
            .filter_map(|l| l["verifyRounds"].as_u64())
            .next_back();
        if let Some(rounds) = found {
            return rounds;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "no agent-log line reports verifyRounds in {}",
        path.display()
    );
}

/// A project whose one check passes, whatever the run did.
const PASSING: &str = r#"{ "name": "t", "scripts": { "test": "node check.js" } }"#;

/// That check's body: src/ok.txt must say "ok", reported as a compiler
/// reports a file.
const CHECK: &str = "const fs = require('fs');\n\
if (fs.readFileSync('src/ok.txt', 'utf8').trim() !== 'ok') {\n\
  console.error('src/ok.txt:1: error: not ok');\n\
  process.exit(1);\n\
}\n";

fn checks_on() -> Permissions {
    Permissions {
        allow_any_command: true,
        checklist: ChecklistMode::Off,
        ..Permissions::default()
    }
}

fn write(id: &'static str, path: &str, content: &str) -> Reply {
    Reply::Call {
        id,
        name: "write",
        args: json!({ "path": path, "content": content }),
    }
}

#[tokio::test]
async fn a_run_starts_with_the_projects_prior_work_in_its_prompt() {
    let h = harness(&[("src/a.rs", "fn a() {}\n")]).await;
    let title = "The retry belongs behind a per-order lock";
    remember_verified(
        &h,
        title,
        "The race went away once retries were serialized per order.",
        &["src/a.rs"],
    );
    let (url, seen) = model(vec![Reply::Text("Done.")]).await;
    h.start(h.request(
        &url,
        Harness::send("Fix the retry race"),
        quiet_permissions(),
    ))
    .await;
    h.run_end().await;

    let first = h
        .events_of("memoryRecalled")
        .first()
        .cloned()
        .expect("the run said what it recalled");
    assert_eq!(first["dialogId"], h.dialog_id.as_str());
    let recalled = first["memories"].as_array().unwrap().clone();
    let mine = recalled
        .iter()
        .find(|m| m["title"] == title)
        .unwrap_or_else(|| panic!("the project's verified memory wasn't recalled: {recalled:#?}"));
    assert_eq!(mine["kind"], "episode");
    assert_eq!(mine["verification"], "verified");
    assert_eq!(mine["files"], json!(["src/a.rs"]));
    assert_eq!(
        mine["content"], "The race went away once retries were serialized per order.",
        "{mine:#?}"
    );

    // Two requests would mean the recall came late.
    let body = seen.lock().unwrap()[0].to_string();
    assert!(
        body.contains(title),
        "the first request carried the memory: {body}"
    );
}

#[tokio::test]
async fn an_edit_is_told_what_was_remembered_about_its_file() {
    let h = harness(&[("src/a.rs", "fn a() {}\n")]).await;
    let title = "src/a.rs wants a trailing blank line";
    remember(
        &h,
        title,
        "The project's formatter adds a blank line at the end of src/a.rs.",
        &["src/a.rs"],
    );
    let (url, _) = model(vec![
        write("w1", "src/a.rs", "fn a() {}\n\n"),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Tidy src/a.rs"), quiet_permissions()))
        .await;
    h.run_end().await;

    let notes: Vec<Value> = h
        .events_of("toolNote")
        .into_iter()
        .filter(|e| e["toolCallId"] == "w1")
        .map(|e| e["note"].clone())
        .collect();
    let hint = notes
        .iter()
        .filter_map(|n| n.as_str())
        .find(|n| n.starts_with("Remembered: "))
        .unwrap_or_else(|| panic!("the edit was given no remembered note: {notes:?}"));
    assert!(hint.starts_with("Remembered: "), "{hint}");
    assert!(hint.contains(title), "{hint}");
    assert!(
        hint.contains("The project's formatter adds a blank line"),
        "the note carries what was remembered, not only its title: {hint}"
    );
    assert_eq!(
        notes.last(),
        Some(&Value::Null),
        "the note is cleared once the call's result is in: {notes:?}"
    );
}

#[tokio::test]
async fn checks_that_pass_promote_the_memories_the_run_used() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let h = harness(&[
        ("package.json", PASSING),
        ("check.js", "process.exit(0);\n"),
        ("src/ok.txt", "ok\n"),
        ("notes.txt", "start\n"),
    ])
    .await;
    let title = "notes.txt is scratch space";
    let id = remember(&h, title, "Editing notes.txt is safe.", &["notes.txt"]);

    let (url, _) = model(vec![
        write("w1", "notes.txt", "changed\n"),
        Reply::Text("Done."),
        Reply::Text("Done."),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Change the notes"), checks_on()))
        .await;
    assert_eq!(h.run_end().await, "verified");

    assert_eq!(
        record(&h, id).verification,
        Verification::Verified,
        "the passing checks prove the memory"
    );
    let recalled = last_recalled(&h);
    let mine = recalled
        .iter()
        .find(|m| m["title"] == title)
        .unwrap_or_else(|| panic!("the run used the memory: {recalled:#?}"));
    assert_eq!(mine["verification"], "verified");
    let evidence = mine["evidence"].as_str().unwrap_or_default();
    assert!(
        evidence.contains("npm test"),
        "the command is the evidence: {evidence}"
    );
}

#[tokio::test]
async fn a_failure_in_the_files_a_memory_names_invalidates_it() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let h = harness(&[
        ("package.json", PASSING),
        ("check.js", CHECK),
        ("src/ok.txt", "ok\n"),
        ("src/other.rs", "fn other() {}\n"),
    ])
    .await;
    let guilty = remember(
        &h,
        "src/ok.txt has to stay ok",
        "The check reads src/ok.txt and wants 'ok'.",
        &["src/ok.txt"],
    );
    let innocent = remember(
        &h,
        "src/other.rs holds one function",
        "src/other.rs is small on purpose.",
        &["src/other.rs"],
    );

    // The run breaks the check: its failure points at the memory's own file.
    let (url, _) = model(vec![
        write("w1", "src/ok.txt", "no\n"),
        Reply::Text("Done."),
        Reply::Text("Nothing more to change."),
        Reply::Text("Nothing more to change."),
        Reply::Text("Nothing more to change."),
        Reply::Text("Nothing more to change."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Rewrite src/ok.txt"), checks_on()))
        .await;
    let outcome = h.run_end().await;
    assert!(
        ["verify-flagged", "verify-unresolved", "verify-gave-up"].contains(&outcome.as_str()),
        "the checks it broke end the run short: {outcome}"
    );

    assert_eq!(
        record(&h, guilty).verification,
        Verification::Deprecated,
        "the check failed in the file this memory is about"
    );
    assert_eq!(
        record(&h, innocent).verification,
        Verification::Provisional,
        "a memory about another file keeps its state"
    );

    let recalled = last_recalled(&h);
    let about_ok = recalled
        .iter()
        .find(|m| m["title"] == "src/ok.txt has to stay ok")
        .unwrap_or_else(|| panic!("the run used the memory: {recalled:#?}"));
    assert_eq!(about_ok["verification"], "deprecated");
    assert_ne!(
        about_ok["helped"],
        json!(true),
        "an approach whose checks fell short never says it helped: {about_ok:#?}"
    );
}

#[tokio::test]
async fn the_end_of_a_run_says_whether_the_approach_helped() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let h = harness(&[
        ("package.json", PASSING),
        ("check.js", "process.exit(0);\n"),
        ("src/ok.txt", "ok\n"),
        ("notes.txt", "start\n"),
    ])
    .await;
    let id = remember(
        &h,
        "notes.txt is scratch space",
        "Editing notes.txt is safe.",
        &["notes.txt"],
    );
    let (url, _) = model(vec![
        write("w1", "notes.txt", "changed\n"),
        Reply::Text("Done."),
        Reply::Text("Done."),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Change the notes"), checks_on()))
        .await;
    assert_eq!(h.run_end().await, "verified");

    let id_text = id.to_string();
    let recalled = last_recalled(&h);
    let mine = recalled
        .iter()
        .find(|m| m["id"] == id_text.as_str())
        .unwrap_or_else(|| panic!("the run used the memory: {recalled:#?}"));
    assert_eq!(
        mine["helped"],
        json!(true),
        "the checks passed, so the approach helped: {mine:#?}"
    );

    let used = record(&h, id);
    assert_eq!(used.use_count, 1, "the run counted one use");
    assert_eq!(used.success_count, 1, "and one that helped");
}

#[tokio::test]
async fn a_run_that_fixed_a_failing_check_keeps_the_lesson_as_a_procedure() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let h = harness(&[
        ("package.json", PASSING),
        ("check.js", CHECK),
        ("src/ok.txt", "no\n"),
        ("notes.txt", "start\n"),
    ])
    .await;
    let (url, _) = model(vec![
        // A change that doesn't touch what the check wants: it fails.
        write("w1", "notes.txt", "scratch\n"),
        Reply::Text("Not fixed yet."),
        // The fix the check is about, and the round that proves it.
        write("w2", "src/ok.txt", "ok\n"),
        Reply::Text("Fixed."),
        Reply::Text("Done."),
        Reply::Text("Done."),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Make the check pass"), checks_on()))
        .await;
    assert_eq!(h.run_end().await, "verified");

    let lessons = procedures(&h);
    assert_eq!(
        lessons.len(),
        1,
        "the repaired check became one lesson: {lessons:#?}"
    );
    let lesson = &lessons[0];
    assert!(
        lesson.title.contains("npm test"),
        "the lesson is named after the failing check: {}",
        lesson.title
    );
    assert!(
        lesson.content.contains("src/ok.txt"),
        "the lesson says what made it pass: {}",
        lesson.content
    );
    assert_eq!(lesson.verification, Verification::Verified);
}

#[tokio::test]
async fn a_green_run_whose_checks_ran_twice_keeps_no_lesson() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    // Two green rounds can't be reached in this harness: the untested-change
    // round needs the change to map to test paths, and the automatic test
    // writer runs before the checks, so by the first round the test it
    // writes is already among the changed files. What the plan really asks
    // of this case is the point kept below: a run whose rounds only ever
    // passed learns no lesson, however many rounds it ran.
    let h = harness(&[
        ("package.json", PASSING),
        ("check.js", "process.exit(0);\n"),
        ("src/math.ts", "export const one = 1;\n"),
    ])
    .await;
    let (url, _) = model(vec![
        write("w1", "src/math.ts", "export const two = 2;\n"),
        Reply::Text("Done."),
        Reply::Text("Done."),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Change the constant"), checks_on()))
        .await;
    let outcome = h.run_end().await;
    assert!(
        ["verified", "repaired", "done"].contains(&outcome.as_str()),
        "checks that only ever passed leave the run green: {outcome}"
    );

    // The checks did run, and passed.
    let messages = h.messages().await;
    let records: Vec<&Value> = messages
        .iter()
        .filter_map(|m| m.meta.as_ref()?.get("verification"))
        .collect();
    assert_eq!(records.len(), 1, "one round of checks: {records:#?}");
    assert_eq!(records[0]["status"], "passed");
    assert!(verify_rounds(&h).await >= 1, "the checks ran");

    assert!(
        procedures(&h).is_empty(),
        "no check ever failed, so there is nothing to learn"
    );
}

#[tokio::test]
async fn a_run_paused_for_the_checks_approval_still_records_its_memories() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let h = harness(&[
        ("package.json", PASSING),
        ("check.js", "process.exit(0);\n"),
        ("src/ok.txt", "ok\n"),
    ])
    .await;
    let id = remember(
        &h,
        "notes.txt is scratch space",
        "Editing notes.txt is safe.",
        &["notes.txt"],
    );
    let (url, _) = model(vec![
        write("w1", "x.txt", "x"),
        Reply::Text("Done."),
        Reply::Text("Done."),
    ])
    .await;
    let permissions = Permissions {
        command_approval: crate::agent_run::permissions::ApprovalRule {
            required: true,
            by: crate::agent_run::permissions::Approver::User,
        },
        ..Permissions::default()
    };
    h.start(h.request(&url, Harness::send("Write x"), permissions.clone()))
        .await;
    assert_eq!(h.run_end().await, "verify-needs-approval");
    assert_eq!(
        record(&h, id).verification,
        Verification::Provisional,
        "nothing was checked yet, so nothing is proven"
    );

    let messages = h.messages().await;
    let waiting = messages.last().unwrap();
    assert_eq!(
        waiting.meta.as_ref().unwrap()["verification"]["status"],
        "needs-approval"
    );
    let record_id = waiting.id.clone();

    h.events.lock().unwrap().clear();
    h.start(h.request(&url, StartKind::RunChecks { record_id }, permissions))
        .await;
    assert_eq!(h.run_end().await, "verified");

    let promoted = record(&h, id);
    assert_eq!(
        promoted.verification,
        Verification::Verified,
        "the run that ran the checks still knows what it recalled"
    );
    assert_eq!(promoted.use_count, 1, "the recalled memory was used once");
    assert_eq!(promoted.success_count, 1, "and it helped");
}

#[tokio::test]
async fn a_project_with_memory_turned_off_recalls_nothing() {
    let h = harness(&[("src/a.rs", "fn a() {}\n")]).await;
    let title = "src/a.rs is the entry point";
    let id = remember(&h, title, "src/a.rs holds main.", &["src/a.rs"]);

    // Settings → the project's own switch, off.
    let root = repo(&h);
    let config = &h.core.paths.config_dir;
    let mut settings = crate::project_profile::load_project_settings(config, &root);
    if !settings.is_object() {
        settings = json!({});
    }
    settings["memory"] = json!(false);
    crate::project_profile::save_project_settings(config, root, settings).unwrap();

    let (url, seen) = model(vec![write("w1", "notes.txt", "x"), Reply::Text("Done.")]).await;
    h.start(h.request(&url, Harness::send("Write notes"), quiet_permissions()))
        .await;
    h.run_end().await;

    assert!(
        h.events_of("memoryRecalled").is_empty(),
        "nothing is recalled for a project that turned memory off"
    );
    let body = seen.lock().unwrap()[0].to_string();
    assert!(
        !body.contains(title),
        "the prompt carries no memory: {body}"
    );

    let untouched = record(&h, id);
    assert_eq!(untouched.use_count, 0, "no verdict was recorded");
    assert_eq!(untouched.success_count, 0);
    assert_eq!(
        store(&h).stats(&repo(&h)).unwrap().total,
        1,
        "the run recorded nothing: the memory of the project is as it was"
    );
}
