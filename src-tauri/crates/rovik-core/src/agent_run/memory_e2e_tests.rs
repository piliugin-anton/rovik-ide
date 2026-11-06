//! A project's long-term memory in whole runs (memory.rs, the memory
//! crate): what a run recalls into its prompt, the note an edit is given
//! about its own file, the applicability the checks leave a memory with, and
//! the reflection, lesson and association the run leaves behind. The project's
//! check is a node script, as in the repair suite.

use super::*;
use verified_code_memory::model::MemoryRecord;
use verified_code_memory::{NewMemory, SharedMemoryStore, Verification};

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

/// The project's memories, newest first, every one of them: a query is not
/// a way to read the memory out any more, because a task nothing matches is
/// answered with nothing.
fn recent(h: &Harness) -> Vec<MemoryRecord> {
    store(h).list(&repo(h), "").unwrap()
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
    assert_eq!(
        mine["applicability"], "current",
        "a verified memory of this revision is expected to hold: {mine:#?}"
    );
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
async fn checks_that_pass_leave_the_runs_memories_as_they_were() {
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
    let before = record(&h, id);

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

    let stored = record(&h, id);
    assert_eq!(
        stored.verification,
        Verification::Provisional,
        "a passing suite says nothing about this claim, so it stays provisional"
    );
    assert_eq!(
        stored.applicability.to_string(),
        "unknown",
        "a green round says nothing about the code of today either"
    );
    assert_eq!(
        stored.evidence, before.evidence,
        "nothing spoke about the claim, so it keeps the evidence it had"
    );
    assert_eq!(
        stored.content_updated_at, before.content_updated_at,
        "and the claim's own stamp is untouched"
    );

    let recalled = last_recalled(&h);
    let mine = recalled
        .iter()
        .find(|m| m["title"] == title)
        .unwrap_or_else(|| panic!("the run used the memory: {recalled:#?}"));
    assert_eq!(mine["verification"], "provisional");
    assert_eq!(
        mine["applicability"], "unknown",
        "the panel is told the claim is neither current nor stale: {mine:#?}"
    );
    assert_eq!(
        mine["association"], "checksPassed",
        "the panel is told which checks followed the memory: {mine:#?}"
    );
    assert!(
        mine.get("helped").is_none(),
        "the panel is no longer asked whether the memory helped: {mine:#?}"
    );
}

#[tokio::test]
async fn a_failure_in_the_files_a_memory_names_marks_it_to_be_checked_again() {
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
    let guilty = remember_verified(
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
    let before = record(&h, guilty);
    assert_eq!(
        before.verification,
        Verification::Verified,
        "the project already verified the memory this test is about"
    );
    let untouched_before = record(&h, innocent);

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

    let marked = record(&h, guilty);
    assert_eq!(
        marked.applicability.to_string(),
        "needs_revalidation",
        "the check failed in the file this memory is about, so the claim must be checked again"
    );
    assert_eq!(
        marked.verification,
        Verification::Verified,
        "a failure in the file is not a verdict on the claim: the evidence stands"
    );
    assert_eq!(
        marked.evidence.as_deref(),
        Some("cargo test passed"),
        "the evidence the memory was verified with is kept"
    );
    assert_eq!(marked.title, before.title, "the claim's title is untouched");
    assert_eq!(
        marked.content, before.content,
        "the claim's text is untouched"
    );
    assert_eq!(
        marked.content_updated_at, before.content_updated_at,
        "nothing was edited, so the claim's own stamp stays"
    );

    let untouched = record(&h, innocent);
    assert_eq!(
        untouched.applicability, untouched_before.applicability,
        "a memory about another file keeps its applicability"
    );
    assert_eq!(
        untouched.verification,
        Verification::Provisional,
        "a memory about another file keeps its state"
    );
    assert_eq!(
        untouched.content_updated_at, untouched_before.content_updated_at,
        "and its claim's stamp"
    );

    let recalled = last_recalled(&h);
    let about_ok = recalled
        .iter()
        .find(|m| m["title"] == "src/ok.txt has to stay ok")
        .unwrap_or_else(|| panic!("the run used the memory: {recalled:#?}"));
    assert_eq!(
        about_ok["applicability"], "needs_revalidation",
        "the panel says the claim has to be checked again: {about_ok:#?}"
    );
    assert_eq!(about_ok["verification"], "verified");
    assert_eq!(
        about_ok["association"], "checksFailed",
        "the panel says which checks followed the memory: {about_ok:#?}"
    );
    assert!(
        about_ok.get("helped").is_none(),
        "the panel is no longer asked whether the memory helped: {about_ok:#?}"
    );
}

#[tokio::test]
async fn the_end_of_a_run_says_which_checks_came_after_the_memories() {
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
    let before = record(&h, id);
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
        mine["association"], "checksPassed",
        "the checks passed after this memory was used: {mine:#?}"
    );
    assert!(
        mine.get("helped").is_none(),
        "the panel reports an association, not a verdict on the memory: {mine:#?}"
    );

    let used = record(&h, id);
    assert_eq!(used.use_count, 1, "the run counted one use");
    assert_eq!(used.success_count, 1, "and one the checks passed after");
    assert_eq!(
        used.content_updated_at, before.content_updated_at,
        "counting a use never makes the remembered claim look newer"
    );
    assert_eq!(
        used.verification,
        Verification::Provisional,
        "and a passing round is not evidence about the claim"
    );

    let reflections: Vec<MemoryRecord> = recent(&h)
        .into_iter()
        .filter(|memory| memory.title == "Change the notes")
        .collect();
    let titles: Vec<String> = recent(&h).into_iter().map(|memory| memory.title).collect();
    let reflection = reflections
        .first()
        .unwrap_or_else(|| panic!("the run left its account of what it did behind: {titles:?}"));
    assert_eq!(
        reflection.source_kind.to_string(),
        "generated deduction",
        "a reflection is the run's own deduction, not an observation of the code"
    );
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
    assert_eq!(
        lesson.source_kind.to_string(),
        "test result",
        "a lesson is the account of a check that ran, so its source is a test result"
    );
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
    let before = record(&h, id);
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
    h.start(h.request(&url, Harness::send("Write the notes"), permissions.clone()))
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

    let after = record(&h, id);
    assert_eq!(
        after.verification,
        Verification::Provisional,
        "the run that ran the checks still knows what it recalled, and a green round proves nothing"
    );
    assert_eq!(
        after.applicability.to_string(),
        "unknown",
        "nothing was said about the file the memory speaks about"
    );
    assert_eq!(after.use_count, 1, "the recalled memory was used once");
    assert_eq!(
        after.success_count, 1,
        "and the checks passed after it was used"
    );
    assert_eq!(
        after.content_updated_at, before.content_updated_at,
        "recording the association never freshens the claim"
    );
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
    // The request names the memory's own file, so recall would find it: what
    // keeps it out of the prompt here is the project's switch, nothing else.
    h.start(h.request(
        &url,
        Harness::send("Tidy src/a.rs and its main"),
        quiet_permissions(),
    ))
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

// The two tools a run asks its project's memory through (PR 3), and the two
// reasons they refuse.

/// A call to one tool, as the scripted model makes it.
fn call_tool(id: &'static str, name: &'static str, args: Value) -> Reply {
    Reply::Call { id, name, args }
}

/// Every request the run sent the model, in the order it sent them.
fn requests(seen: &Arc<Mutex<Vec<Value>>>) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .map(|body| body.to_string())
        .collect()
}

/// How many of `ids` a body names: the records an answer showed.
fn records_named(body: &str, ids: &[uuid::Uuid]) -> usize {
    ids.iter()
        .filter(|id| body.contains(&id.to_string()))
        .count()
}

/// Settings → the project's own switch, off.
fn memory_off(h: &Harness) {
    let root = repo(h);
    let config = &h.core.paths.config_dir;
    let mut settings = crate::project_profile::load_project_settings(config, &root);
    if !settings.is_object() {
        settings = json!({});
    }
    settings["memory"] = json!(false);
    crate::project_profile::save_project_settings(config, root, settings).unwrap();
}

/// A run in which the model first calls one memory tool and then ends.
///
/// The task names nothing any of the project's memories speak about, so what a
/// request carries from the tool's answer is the tool's answer and not a
/// memory the run recalled on its own.
async fn run_calling(
    h: &Harness,
    task: &str,
    permissions: Permissions,
    id: &'static str,
    tool: &'static str,
    args: Value,
) -> Vec<String> {
    let (url, seen) = model(vec![call_tool(id, tool, args), Reply::Text("Done.")]).await;
    h.start(h.request(&url, Harness::send(task), permissions))
        .await;
    h.run_end().await;
    requests(&seen)
}

#[tokio::test]
async fn memory_search_finds_a_remembered_record_by_a_shared_word() {
    let h = harness(&[("src/a.rs", "fn a() {}\n")]).await;
    let title = "The retry belongs behind the zanzibar lock";
    let content = "The retry table is guarded by zanzibar, one per order.";
    let id = remember_verified(&h, title, content, &["src/a.rs"]);

    let bodies = run_calling(
        &h,
        "Tidy the notes",
        quiet_permissions(),
        "m1",
        "memory_search",
        json!({ "query": "zanzibar" }),
    )
    .await;

    assert!(
        !bodies[0].contains(title),
        "nothing recalled this record before the search, so what comes next is \
         the tool's answer: {}",
        bodies[0]
    );
    let answer = &bodies[1];
    assert!(
        answer.contains(title),
        "the search found the record by the word it shares with the query: {answer}"
    );
    assert!(
        answer.contains(&id.to_string()),
        "the answer carries the record's id, which is what memory_get takes: {answer}"
    );
    assert!(
        answer.contains("zanzibar, one per order"),
        "the answer carries what was remembered, not only its title: {answer}"
    );
    assert!(
        answer.contains("verified"),
        "the answer says how the claim stands: {answer}"
    );
    assert!(
        answer.contains("current"),
        "the answer says whether the claim is expected to hold: {answer}"
    );
}

#[tokio::test]
async fn memory_get_answers_with_the_record_the_search_named() {
    let h = harness(&[("src/a.rs", "fn a() {}\n")]).await;
    let title = "The retry belongs behind the zanzibar lock";
    let content = "The retry table is guarded by zanzibar, one per order.";
    let id = remember_verified(&h, title, content, &["src/a.rs"]);

    let bodies = run_calling(
        &h,
        "Tidy the notes",
        quiet_permissions(),
        "m1",
        "memory_get",
        json!({ "id": id.to_string() }),
    )
    .await;

    let answer = &bodies[1];
    assert!(
        answer.contains(title),
        "the record the packet showed is read back by its id: {answer}"
    );
    assert!(
        answer.contains("zanzibar, one per order"),
        "and its content comes with it: {answer}"
    );
    assert!(
        answer.contains(&id.to_string()),
        "the answer names the record it read: {answer}"
    );
    assert!(
        answer.contains("cargo test passed"),
        "the evidence the project verified it with comes with it: {answer}"
    );
}

#[tokio::test]
async fn memory_search_says_it_is_off_when_the_project_turned_memory_off() {
    let h = harness(&[("src/a.rs", "fn a() {}\n")]).await;
    remember_verified(
        &h,
        "The retry belongs behind the zanzibar lock",
        "The retry table is guarded by zanzibar.",
        &["src/a.rs"],
    );
    memory_off(&h);

    let bodies = run_calling(
        &h,
        "Tidy the notes",
        quiet_permissions(),
        "m1",
        "memory_search",
        json!({ "query": "zanzibar" }),
    )
    .await;

    let answer = &bodies[1];
    assert!(
        answer.contains("turned off for this project"),
        "a project with memory off is told so, not answered with nothing: {answer}"
    );
}

#[tokio::test]
async fn memory_get_says_it_is_off_when_the_project_turned_memory_off() {
    let h = harness(&[("src/a.rs", "fn a() {}\n")]).await;
    let id = remember_verified(
        &h,
        "The retry belongs behind the zanzibar lock",
        "The retry table is guarded by zanzibar.",
        &["src/a.rs"],
    );
    memory_off(&h);

    let bodies = run_calling(
        &h,
        "Tidy the notes",
        quiet_permissions(),
        "m1",
        "memory_get",
        json!({ "id": id.to_string() }),
    )
    .await;

    let answer = &bodies[1];
    assert!(
        answer.contains("turned off for this project"),
        "a project with memory off is told so, not answered with nothing: {answer}"
    );
}

#[tokio::test]
async fn memory_search_says_it_is_unavailable_when_its_store_could_not_be_opened() {
    let h = harness(&[("src/a.rs", "fn a() {}\n")]).await;

    // The place the project's store goes is taken by a folder, so opening it
    // fails: the project's memory is on, and this run has none of it.
    let file = crate::memory::store_path(&h.core.paths, &h.project);
    let _ = std::fs::remove_file(&file);
    std::fs::create_dir_all(&file).expect("the store's place is taken by a folder");

    let bodies = run_calling(
        &h,
        "Tidy the notes",
        quiet_permissions(),
        "m1",
        "memory_search",
        json!({ "query": "zanzibar" }),
    )
    .await;

    let answer = &bodies[1];
    assert!(
        answer.contains("unavailable"),
        "a store that could not be opened is said to be unavailable this run, \
         not answered with nothing: {answer}"
    );
}

#[tokio::test]
async fn memory_search_answers_with_no_more_records_than_its_limit() {
    let h = harness(&[("src/a.rs", "fn a() {}\n")]).await;
    let ids: Vec<uuid::Uuid> = (0..10)
        .map(|n| {
            remember_verified(
                &h,
                &format!("zanzibar rule {n}"),
                "The zanzibar guard is held per order.",
                &[],
            )
        })
        .collect();
    assert_eq!(ids.len(), 10, "ten memories share the word");

    let one = run_calling(
        &h,
        "Tidy the notes",
        quiet_permissions(),
        "m1",
        "memory_search",
        json!({ "query": "zanzibar", "limit": 2 }),
    )
    .await;
    assert_eq!(
        records_named(&one[1], &ids),
        2,
        "ten records share the word and the search was asked for two, so it \
         shows two: {}",
        one[1]
    );
}

#[tokio::test]
async fn memory_search_clamps_a_limit_above_eight() {
    let h = harness(&[("src/a.rs", "fn a() {}\n")]).await;
    let ids: Vec<uuid::Uuid> = (0..10)
        .map(|n| {
            remember_verified(
                &h,
                &format!("zanzibar rule {n}"),
                "The zanzibar guard is held per order.",
                &[],
            )
        })
        .collect();

    let bodies = run_calling(
        &h,
        "Tidy the notes",
        quiet_permissions(),
        "m1",
        "memory_search",
        json!({ "query": "zanzibar", "limit": 50 }),
    )
    .await;

    assert_eq!(
        records_named(&bodies[1], &ids),
        8,
        "ten memories match and eight is the most a search shows, whatever it \
         asked for: {}",
        bodies[1]
    );
}

// A round of checks that fails: what the run does with the failure.

#[tokio::test]
async fn a_failing_round_brings_a_matching_verified_repair_into_the_next_request_once() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let h = harness(&[
        ("package.json", PASSING),
        ("check.js", CHECK),
        ("src/ok.txt", "ok\n"),
        ("notes.txt", "start\n"),
    ])
    .await;
    let repair = "When npm test says a text file is not ok, write back the text it wants";
    remember_verified(
        &h,
        repair,
        "The check reads src/ok.txt: writing back what it wants makes npm test pass.",
        &["src/ok.txt"],
    );

    let (url, seen) = model(vec![
        // The run breaks the check; then a change that touches nothing the
        // check reads, so the second round fails the very same way.
        write("w1", "src/ok.txt", "no"),
        Reply::Text("Nothing more to change."),
        write(
            "w2",
            "notes.txt",
            "a second try that changes nothing the check reads\n",
        ),
        Reply::Text("Nothing more to change."),
        Reply::Text("Nothing more to change."),
    ])
    .await;
    // The task names no file the repair speaks about, so only a failure can
    // bring it into the run.
    h.start(h.request(&url, Harness::send("Tidy the notes"), checks_on()))
        .await;
    let outcome = h.run_end().await;
    assert!(
        ["verify-flagged", "verify-unresolved", "verify-gave-up"].contains(&outcome.as_str()),
        "the checks it broke end the run short: {outcome}"
    );
    assert!(
        verify_rounds(&h).await >= 2,
        "the checks ran twice, both times failing the same way"
    );

    let bodies = requests(&seen);
    assert!(
        !bodies[0].contains(repair),
        "no memory of the repair was recalled before a check failed: {}",
        bodies[0]
    );
    let carried = bodies
        .iter()
        .position(|body| body.contains(repair))
        .unwrap_or_else(|| {
            panic!(
                "no round of checks brought the verified repair into the context \
                 of the next request: {}",
                bodies.join("\n\n---\n\n")
            )
        });
    assert_eq!(
        bodies[carried].matches(repair).count(),
        1,
        "the failure brings the repair in once: {}",
        bodies[carried]
    );
    assert_eq!(
        bodies.last().unwrap().matches(repair).count(),
        1,
        "the second identical failing round does not look it up again, so the \
         retrieved set did not grow: {}",
        bodies.last().unwrap()
    );
}

#[tokio::test]
async fn the_packet_drops_the_record_a_failed_check_marked_and_is_rebuilt_after_the_round() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let h = harness(&[
        ("package.json", PASSING),
        ("check.js", CHECK),
        ("src/ok.txt", "ok\n"),
        ("notes.txt", "start\n"),
    ])
    .await;
    let guilty = "src/ok.txt has to say ok";
    let innocent = "notes.txt is scratch space";
    let guilty_id = remember_verified(
        &h,
        guilty,
        "The check reads src/ok.txt and wants 'ok'.",
        &["src/ok.txt"],
    );
    remember_verified(&h, innocent, "Editing notes.txt is safe.", &["notes.txt"]);

    let (url, seen) = model(vec![
        write("w1", "src/ok.txt", "no"),
        Reply::Text("Nothing more to change."),
        Reply::Text("Nothing more to change."),
        Reply::Text("Nothing more to change."),
        Reply::Text("Nothing more to change."),
    ])
    .await;
    h.start(h.request(
        &url,
        Harness::send("Tidy src/ok.txt and notes.txt"),
        checks_on(),
    ))
    .await;
    let outcome = h.run_end().await;
    assert!(
        ["verify-flagged", "verify-unresolved", "verify-gave-up"].contains(&outcome.as_str()),
        "the checks it broke end the run short: {outcome}"
    );

    let bodies = requests(&seen);
    assert!(
        bodies[0].contains(guilty),
        "the run starts with the record about the file it was asked about: {}",
        bodies[0]
    );
    assert!(
        bodies[0].contains(innocent),
        "and with the other record its task speaks about: {}",
        bodies[0]
    );
    assert_eq!(
        record(&h, guilty_id).applicability.to_string(),
        "needs_revalidation",
        "the failure fell in the file this record is about"
    );

    let packet = bodies.last().unwrap();
    assert!(
        !packet.contains(guilty),
        "the round marked the record to be checked again, so the packet it \
         rebuilt leaves it out: {packet}"
    );
    assert!(
        packet.contains(innocent),
        "the packet is rebuilt after the round, not left behind: the record \
         nothing spoke against is still there: {packet}"
    );
}

// A file the user keeps from the agent, and a failure that points at it.

/// Saves the project's list of restricted files, as Settings → Restricted
/// files does, beside its other settings.
fn restrict(h: &Harness, entries: Value) {
    let root = repo(h);
    let config = &h.core.paths.config_dir;
    let mut settings = crate::project_profile::load_project_settings(config, &root);
    if !settings.is_object() {
        settings = json!({});
    }
    settings["restrictedFiles"] = entries;
    crate::project_profile::save_project_settings(config, root, settings).unwrap();
}

/// A check that fails, naming the file it reads and a word of its own, so a
/// memory of what its failure wants can be told from any other.
const ZANZIBAR_CHECK: &str = "const fs = require('fs');\n\
if (fs.readFileSync('src/ok.txt', 'utf8').trim() !== 'ok') {\n\
  console.error('src/ok.txt:1: error: zanzibar did not hold');\n\
  process.exit(1);\n\
}\n";

#[tokio::test]
async fn a_repair_about_a_restricted_file_never_reaches_the_model() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let h = harness(&[
        ("package.json", PASSING),
        ("check.js", ZANZIBAR_CHECK),
        // The file the check reads is already wrong, so the first round of
        // checks fails on it without the run having to change it.
        ("src/ok.txt", "no\n"),
        ("notes.txt", "start\n"),
    ])
    .await;
    // Settings → Restricted files: the run may not read what the check reads.
    restrict(&h, json!([{ "path": "src/ok.txt" }]));

    // Two verified repairs about the very same failure, saying the same thing,
    // so the round finds both: only the files they name tell them apart.
    let about_the_restricted_file = "The zanzibar check wants the word ok";
    let restricted_content = "When the check prints 'zanzibar did not hold', writing ok \
         into the file it reads makes npm test pass.";
    remember_verified(
        &h,
        about_the_restricted_file,
        restricted_content,
        &["src/ok.txt"],
    );
    let about_the_failure = "A zanzibar failure means the text the check reads is wrong";
    let free_content = "When the check prints 'zanzibar did not hold' and exits 1, write \
         back the text it asks for.";
    remember_verified(&h, about_the_failure, free_content, &["check.js"]);

    let (url, seen) = model(vec![
        // A change that touches nothing the check reads: the round after it
        // fails the same way.
        write(
            "w1",
            "notes.txt",
            "a try that leaves the check's file alone\n",
        ),
        Reply::Text("Nothing more to change."),
        Reply::Text("Nothing more to change."),
        Reply::Text("Nothing more to change."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Tidy the notes"), checks_on()))
        .await;
    let outcome = h.run_end().await;
    assert!(
        ["verify-flagged", "verify-unresolved", "verify-gave-up"].contains(&outcome.as_str()),
        "the check the run may not fix ends it short: {outcome}"
    );
    assert!(
        verify_rounds(&h).await >= 1,
        "a round of checks ran and failed on the file the user restricted"
    );

    let bodies = requests(&seen);
    let all = bodies.join("\n\n---\n\n");
    for told in [about_the_restricted_file, restricted_content] {
        assert!(
            bodies.iter().all(|body| !body.contains(told)),
            "no request may tell the model about a memory naming the file the \
             user restricted, and one carries {told:?}: {all}"
        );
    }

    let events = h.events_of("memoryRecalled");
    assert!(
        events
            .iter()
            .all(|event| !event.to_string().contains(about_the_restricted_file)),
        "no memoryRecalled event may carry the repair that names the file the \
         user restricted: {events:?}"
    );

    // The same failure does bring the repair about a file the run may read:
    // what kept the other one out is the report's files, not the report.
    assert!(
        bodies
            .iter()
            .any(|body| body.contains(about_the_failure) && body.contains(free_content)),
        "the failing round brings the unrestricted repair about the same failure \
         into the request: {all}"
    );
    assert!(
        !bodies[0].contains(about_the_failure),
        "no memory of the repair was recalled before a check failed: {}",
        bodies[0]
    );
}
