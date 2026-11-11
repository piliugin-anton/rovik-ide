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
async fn successful_parallel_reads_supply_distinct_file_memories_before_the_next_decision() {
    let h = harness(&[("src/a.rs", "fn a() {}\n"), ("src/b.rs", "fn b() {}\n")]).await;
    for _ in 0..8 {
        remember_verified(
            &h,
            "Zanzibar guard",
            "Serialize zanzibar retries.",
            &["src/a.rs"],
        );
    }
    remember_verified(
        &h,
        "Yggdrasil invariant",
        "Retain the yggdrasil boundary.",
        &["src/b.rs"],
    );
    let (url, seen) = model(vec![
        Reply::Calls(vec![
            ("r1", "read", json!({"path":"src/a.rs"})),
            ("r2", "read", json!({"path":"src/b.rs"})),
        ]),
        Reply::Call {
            id: "r3",
            name: "read",
            args: json!({"path":"./src/a.rs"}),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(
        &url,
        Harness::send("Inspect the implementation"),
        quiet_permissions(),
    ))
    .await;
    assert_eq!(h.run_end().await, "done");
    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(!requests[0].to_string().contains("Zanzibar guard"));
    for request in &requests[1..] {
        let body = request.to_string();
        assert_eq!(body.matches("Zanzibar guard").count(), 1, "{body}");
        assert!(body.contains("Yggdrasil invariant"), "{body}");
    }
    assert_eq!(last_recalled(&h).len(), 2);
    assert_eq!(
        h.events_of("memoryRecalled").len(),
        4,
        "initial recall, two observed files, run end; repeated read is cached"
    );
}

#[tokio::test]
async fn search_hits_supply_file_context_without_parsing_untrusted_result_text() {
    let h = harness(&[("src/a.rs", "fn unique_search_symbol() {}\n")]).await;
    remember_verified(
        &h,
        "Zanzibar invariant",
        "Retain the zanzibar boundary.",
        &["src/a.rs"],
    );
    let (url, seen) = model(vec![
        Reply::Call {
            id: "s1",
            name: "search_code",
            args: json!({"query":"unique_search_symbol"}),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(
        &url,
        Harness::send("Investigate this behavior"),
        quiet_permissions(),
    ))
    .await;
    assert_eq!(h.run_end().await, "done");
    let requests = seen.lock().unwrap();
    assert!(!requests[0].to_string().contains("Zanzibar invariant"));
    assert!(requests[1].to_string().contains("Zanzibar invariant"));
}

#[tokio::test]
async fn observed_file_retrieval_rejects_restricted_and_provisional_candidates_before_limits() {
    let h = harness(&[("src/a.rs", "fn a() {}\n")]).await;
    for n in 0..8 {
        remember_verified(
            &h,
            &format!("Hidden boundary {n}"),
            "Hidden implementation detail.",
            &["src/a.rs", ".env"],
        );
    }
    remember(
        &h,
        "Unproven boundary",
        "An untested assertion.",
        &["src/a.rs"],
    );
    remember_verified(
        &h,
        "Visible boundary",
        "A supported convention.",
        &["src/a.rs"],
    );
    let (url, seen) = model(vec![
        Reply::Call {
            id: "r1",
            name: "read",
            args: json!({"path":"src/a.rs"}),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(
        &url,
        Harness::send("Inspect the implementation"),
        quiet_permissions(),
    ))
    .await;
    assert_eq!(h.run_end().await, "done");
    let body = seen.lock().unwrap()[1].to_string();
    assert!(body.contains("Visible boundary"), "{body}");
    assert!(!body.contains("Hidden boundary"), "{body}");
    assert!(!body.contains("Unproven boundary"), "{body}");
}

#[tokio::test]
async fn a_failed_read_does_not_supply_file_memory() {
    let h = harness(&[("src/a.rs", "fn a() {}\n")]).await;
    remember_verified(
        &h,
        "Missing boundary",
        "An old observation.",
        &["src/missing.rs"],
    );
    let (url, seen) = model(vec![
        Reply::Call {
            id: "r1",
            name: "read",
            args: json!({"path":"src/missing.rs"}),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(
        &url,
        Harness::send("Inspect the implementation"),
        quiet_permissions(),
    ))
    .await;
    assert_eq!(h.run_end().await, "done");
    assert!(!seen.lock().unwrap()[1]
        .to_string()
        .contains("Missing boundary"));
}

#[tokio::test]
async fn the_same_claim_from_two_source_files_keeps_both_provisional_observations() {
    let h = harness(&[("src/a.rs", "fn a() {}\n"), ("src/b.rs", "fn b() {}\n")]).await;
    let(url,_)=model(vec![Reply::Calls(vec![
        ("m1","memory_remember",json!({"kind":"fact","title":"Shared convention","claim":"Guard state updates.","applies_to":["src/a.rs"],"evidence":"src/a.rs:1 source observation"})),
        ("m2","memory_remember",json!({"kind":"fact","title":"Shared convention","claim":"Guard state updates.","applies_to":["src/b.rs"],"evidence":"src/b.rs:1 source observation"})),
    ]),Reply::Text("Done.")]).await;
    h.start(h.request(
        &url,
        Harness::send("Inspect the source"),
        quiet_permissions(),
    ))
    .await;
    assert_eq!(h.run_end().await, "done");
    let records = records_titled(&h, "Shared convention");
    assert_eq!(records.len(), 2, "{records:#?}");
    assert!(records
        .iter()
        .all(|record| record.verification == Verification::Provisional));
    assert!(records
        .iter()
        .any(|record| record.evidence.as_deref() == Some("src/a.rs:1 source observation")));
    assert!(records
        .iter()
        .any(|record| record.evidence.as_deref() == Some("src/b.rs:1 source observation")));
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
async fn explicit_reference_metadata_supplies_file_hints_without_becoming_the_recall_task() {
    let h = harness(&[
        ("src/explicit.rs", "fn explicit() {}\n"),
        ("src/open.rs", "fn open() {}\n"),
    ])
    .await;
    let title = "Zanzibar ownership constraint";
    remember_verified(
        &h,
        title,
        "Retain per-order token ownership.",
        &["src/explicit.rs"],
    );
    let noise = "Zanzibar editor wrapper distraction";
    remember_verified(
        &h,
        noise,
        "Active editor context keeps the cursor at a line.",
        &["src/open.rs"],
    );
    let typed = "Locate the frobnicator";
    let wire = format!(
        "[Active editor context — file src/open.rs (rust); cursor at line 1, col 1.]\n\nThe user explicitly referenced these project files:\n- src/explicit.rs\n\n{typed}"
    );
    let (url, seen) = model(vec![Reply::Text("Done.")]).await;
    let start = StartKind::Send {
        message: UserMessage {
            id: uuid::Uuid::new_v4().to_string(),
            content: typed.to_string(),
            meta: Some(json!({"sentContent":wire})),
            attachments: Vec::new(),
        },
    };
    h.start(h.request(&url, start, quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");
    let first = seen.lock().unwrap()[0].to_string();
    assert!(
        first.contains(title),
        "file metadata was not used for recall: {first}"
    );
    assert!(
        !first.contains(noise),
        "editor wrapper words became recall subjects: {first}"
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

// PR 4: what a run writes back into the project's memory when it ends, and
// the tool a model may record with.

/// A check that reads two files and fails on the first one that is wrong,
/// naming it, so a failure on one is told from a failure on the other.
const TWO_FILES_CHECK: &str = "const fs = require('fs');\n\
for (const [path, want] of [['src/checkout.txt', 'checkout'], ['src/queue.txt', 'queue']]) {\n\
  if (fs.readFileSync(path, 'utf8').trim() !== want) {\n\
    console.error(`${path}:1: error: ${path} does not say ${want}`);\n\
    process.exit(1);\n\
  }\n\
}\n";

/// The memories of the project titled `title`.
fn records_titled(h: &Harness, title: &str) -> Vec<MemoryRecord> {
    recent(h)
        .into_iter()
        .filter(|memory| memory.title == title)
        .collect()
}

/// A command that runs until the test lets it end: it writes `started` in the
/// project, then waits for `go` there — e2e_tests's own helper, for the same
/// reason (on Windows, in cmd alone, a PowerShell started for the wait could
/// take longer to start than the test waits for the run).
fn until_go() -> &'static str {
    if cfg!(windows) {
        "echo ran> started && for /L %i in (0,0,1) do @if exist go exit 0"
    } else {
        "echo ran > started; while [ ! -e go ]; do sleep 0.05; done"
    }
}

/// Waits until the `until_go` command runs in `root`.
async fn command_started(root: &std::path::Path) {
    for _ in 0..400 {
        if root.join("started").exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the command never started");
}

#[tokio::test]
async fn two_failures_of_the_same_check_leave_two_lessons_that_both_stay() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let h = harness(&[
        ("package.json", PASSING),
        ("check.js", TWO_FILES_CHECK),
        ("src/checkout.txt", "no\n"),
        ("src/queue.txt", "queue\n"),
        ("notes.txt", "start\n"),
    ])
    .await;

    // A run whose check fails on the checkout file, and that fixes that file.
    let (url, _) = model(vec![
        write("w1", "notes.txt", "a change that leaves the check alone\n"),
        Reply::Text("Not fixed yet."),
        write("w2", "src/checkout.txt", "checkout\n"),
        Reply::Text("Fixed the checkout."),
        Reply::Text("Done."),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(
        &url,
        Harness::send("Make the checkout check pass"),
        checks_on(),
    ))
    .await;
    assert_eq!(h.run_end().await, "verified");
    assert_eq!(
        procedures(&h).len(),
        1,
        "the repaired check became one lesson: {:#?}",
        procedures(&h)
    );

    // A later run whose check fails on the queue file, and that fixes that one.
    let (url, _) = model(vec![
        write("w3", "src/queue.txt", "not the word the check wants\n"),
        Reply::Text("Not fixed yet."),
        write("w4", "src/queue.txt", "queue\n"),
        Reply::Text("Fixed the queue."),
        Reply::Text("Done."),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(
        &url,
        Harness::send("Make the queue check pass"),
        checks_on(),
    ))
    .await;
    assert_eq!(h.run_end().await, "verified");

    let lessons = procedures(&h);
    assert_eq!(
        lessons.len(),
        2,
        "one command failing on two different files is two lessons: {lessons:#?}"
    );
    for lesson in &lessons {
        assert_ne!(
            lesson.verification,
            Verification::Deprecated,
            "neither failure's lesson replaced the other's: {lesson:#?}"
        );
    }
    let about = |file: &str| lessons.iter().filter(|l| l.content.contains(file)).count();
    assert_eq!(
        about("src/checkout.txt"),
        1,
        "one lesson is the checkout failure's: {lessons:#?}"
    );
    assert_eq!(
        about("src/queue.txt"),
        1,
        "and one is the queue failure's: {lessons:#?}"
    );
    assert_ne!(
        lessons[0].identity, lessons[1].identity,
        "two failures are two identities: {lessons:#?}"
    );
    assert_eq!(
        recent(&h)
            .into_iter()
            .filter(|memory| memory.kind.to_string() == "procedure")
            .count(),
        2,
        "both stay in the project's memory: {:#?}",
        recent(&h)
    );
}

#[tokio::test]
async fn replaying_the_same_run_end_writes_no_second_lesson_and_no_second_reflection() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let h = harness(&[
        ("package.json", PASSING),
        ("check.js", CHECK),
        ("src/ok.txt", "no\n"),
    ])
    .await;

    // A run whose first round fails on what the check reads, and whose second
    // round passes because the model wrote it.
    let (url, _) = model(vec![
        write("w1", "src/ok.txt", "still not what the check wants\n"),
        Reply::Text("Not fixed yet."),
        write("w2", "src/ok.txt", "ok\n"),
        Reply::Text("Fixed."),
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
    let lesson_id = lessons[0].id;
    let lesson_stamp = lessons[0].content_updated_at;
    let after_first = store(&h).stats(&repo(&h)).unwrap().total;
    assert!(
        after_first > 1,
        "the run left its lesson and its account of what it did: {after_first}"
    );

    // The very same run again, in the same chat: the same failure, the same
    // fix, so the end has nothing new to say.
    let (url, _) = model(vec![
        write("w3", "src/ok.txt", "still not what the check wants\n"),
        Reply::Text("Not fixed yet."),
        write("w4", "src/ok.txt", "ok\n"),
        Reply::Text("Fixed."),
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
        "the same failure fixed the same way is one lesson, however often the run \
         ends: {lessons:#?}"
    );
    let same = record(&h, lesson_id);
    assert_eq!(
        same.content_updated_at, lesson_stamp,
        "and the lesson it already had was not revised"
    );
    assert_ne!(
        same.verification,
        Verification::Deprecated,
        "nor superseded by itself"
    );
    assert_eq!(
        store(&h).stats(&repo(&h)).unwrap().total,
        after_first,
        "nothing was written the second time: no lesson and no second account of \
         the run ({:#?})",
        recent(&h)
    );
}

#[tokio::test]
async fn a_newer_repair_supersedes_the_older_account_of_that_failure_only() {
    if !has_npm() {
        eprintln!("npm isn't installed: skipped");
        return;
    }
    let h = harness(&[
        ("package.json", PASSING),
        ("check.js", CHECK),
        ("src/ok.txt", "no\n"),
    ])
    .await;

    // A run that repairs the check: the lesson it writes carries the identity
    // a later repair of the same failure is recognised by.
    let (url, _) = model(vec![
        write("w1", "src/ok.txt", "still not what the check wants\n"),
        Reply::Text("Not fixed yet."),
        write("w2", "src/ok.txt", "ok\n"),
        Reply::Text("Fixed."),
        Reply::Text("Done."),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Make the check pass"), checks_on()))
        .await;
    assert_eq!(h.run_end().await, "verified");

    let first = procedures(&h);
    assert_eq!(first.len(), 1, "one lesson: {first:#?}");
    let identity = first[0].identity.clone();
    assert!(
        identity.starts_with("repair:"),
        "a lesson says which repair it is: {identity}"
    );
    let written = first[0].id;

    // The project's memory, as an earlier run worded it: one older account of
    // this same failure, and one of a failure of its own.
    let (older, elsewhere) = {
        let now = store(&h);
        now.delete(written).unwrap();
        let older = now
            .remember(
                NewMemory::procedure(
                    repo(&h),
                    "npm test fails on src/ok.txt",
                    "Write back the text the check asks for.",
                )
                .with_files(["src/ok.txt"])
                .with_identity(identity.clone()),
            )
            .unwrap()
            .id;
        let elsewhere = now
            .remember(
                NewMemory::procedure(
                    repo(&h),
                    "npm run lint fails on src/notes.txt",
                    "Drop the trailing spaces the linter named.",
                )
                .with_files(["src/notes.txt"])
                .with_identity("repair:npm run lint|src/notes.txt|src/notes.txt"),
            )
            .unwrap()
            .id;
        (older, elsewhere)
    };
    let untouched_before = record(&h, elsewhere);

    // The same failure, repaired and proved again.
    let (url, _) = model(vec![
        write("w3", "src/ok.txt", "no\n"),
        Reply::Text("Not fixed yet."),
        write("w4", "src/ok.txt", "ok\n"),
        Reply::Text("Fixed."),
        Reply::Text("Done."),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Make the check pass"), checks_on()))
        .await;
    assert_eq!(h.run_end().await, "verified");

    let superseded = record(&h, older);
    assert_eq!(
        superseded.verification,
        Verification::Deprecated,
        "the older account of this failure is superseded by the newer one: \
         {superseded:#?}"
    );
    let of_this_identity = store(&h).find_by_identity(&repo(&h), &identity).unwrap();
    assert_eq!(
        of_this_identity.len(),
        2,
        "the superseded account is kept beside the one that replaced it: \
        {of_this_identity:#?}"
    );
    assert_eq!(
        of_this_identity[0].verification,
        Verification::Verified,
        "the repair the run just made is the one the project relies on: \
         {:#?}",
        of_this_identity[0]
    );
    assert_eq!(
        of_this_identity[1].id, older,
        "and the older one is the record it replaced: {of_this_identity:#?}"
    );

    let untouched_after = record(&h, elsewhere);
    assert_eq!(
        untouched_after.verification, untouched_before.verification,
        "a repair of another failure is no part of this one's supersession: \
         {untouched_after:#?}"
    );
    assert_eq!(
        untouched_after.content_updated_at, untouched_before.content_updated_at,
        "and its claim is as it was"
    );
}

#[tokio::test]
async fn a_run_whose_memory_was_cleared_before_its_end_writes_nothing_back() {
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
    // The project's memory holds something before the run starts, so a clear
    // during the run is the clear of something.
    remember(
        &h,
        "src/ok.txt is the file the check reads",
        "The check reads src/ok.txt.",
        &["src/ok.txt"],
    );

    let (url, _) = model(vec![
        // A command that keeps the run at work while the memory is cleared.
        Reply::Call {
            id: "r1",
            name: "bash",
            args: json!({ "command": until_go() }),
        },
        write("w1", "notes.txt", "a change that leaves the check alone\n"),
        Reply::Text("Not fixed yet."),
        write("w2", "src/ok.txt", "ok\n"),
        Reply::Text("Fixed."),
        Reply::Text("Done."),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Make the check pass"), checks_on()))
        .await;

    // The run is at work, its memory in hand: Settings → Clear memory.
    command_started(&h.project).await;
    let cleared = store(&h).clear(&repo(&h)).unwrap();
    assert_eq!(cleared, 1, "the memory the project had was cut down");
    std::fs::write(h.project.join("go"), "").unwrap();

    let outcome = h.run_end().await;
    assert!(
        ["verified", "repaired"].contains(&outcome.as_str()),
        "the run went on to fix the check: {outcome}"
    );
    assert_eq!(
        store(&h).stats(&repo(&h)).unwrap().total,
        0,
        "a run whose memory was cleared before its end writes nothing back \
         ({:#?})",
        recent(&h)
    );
    assert!(
        recent(&h).is_empty(),
        "no lesson and no account of the run came back from what was deleted: \
         {:#?}",
        recent(&h)
    );
}

#[tokio::test]
async fn memory_remember_records_a_provisional_fact_with_its_evidence() {
    let h = harness(&[("src/a.rs", "fn a() {}\n")]).await;
    let title = "The retry table is guarded";
    let claim = "The retry table is guarded by the zanzibar lock.";

    let bodies = run_calling(
        &h,
        "Tidy the notes",
        quiet_permissions(),
        "m1",
        "memory_remember",
        json!({
            "kind": "fact",
            "title": title,
            "claim": claim,
            "applies_to": ["src/a.rs"],
            "evidence": "src/a.rs:3 shows the lock around the table",
        }),
    )
    .await;
    assert_eq!(
        bodies.len(),
        2,
        "the model was asked once more, with the tool's answer"
    );

    let found = records_titled(&h, title);
    assert_eq!(
        found.len(),
        1,
        "the tool recorded one memory of the project: {found:#?}"
    );
    let memory = &found[0];
    assert_eq!(memory.content, claim, "it says what the model claimed");
    assert_eq!(
        memory.verification,
        Verification::Provisional,
        "a model's tool may not verify, so what it records is provisional: \
         {memory:#?}"
    );
    assert_eq!(
        memory.source_kind.to_string(),
        "code observation",
        "the model gave evidence, so the memory is what the run observed: \
         {memory:#?}"
    );
    assert!(
        memory.kind.to_string().contains("fact"),
        "a fact, not a lesson: {}",
        memory.kind
    );
    assert!(
        format!("{:?}", memory.files).contains("src/a.rs"),
        "it applies to the file the model named: {:?}",
        memory.files
    );
    assert_eq!(
        memory.identity,
        format!(
            "fact:{}",
            verified_code_memory::model::claim_hash(title, claim)
        ),
        "a fact the tool recorded is the claim it says, once"
    );

    // Without evidence the same claim is the run's own deduction, not
    // something it saw.
    let other = "The queue is drained once a turn.";
    let other_title = "The queue is drained once a turn";
    run_calling(
        &h,
        "Tidy the notes",
        quiet_permissions(),
        "m2",
        "memory_remember",
        json!({ "kind": "fact", "title": other_title, "claim": other }),
    )
    .await;
    let deduced = records_titled(&h, other_title);
    assert_eq!(deduced.len(), 1, "{deduced:#?}");
    assert_eq!(
        deduced[0].source_kind.to_string(),
        "generated deduction",
        "with no evidence the memory is the run's own deduction: {:#?}",
        deduced[0]
    );
    assert_eq!(
        deduced[0].verification,
        Verification::Provisional,
        "and it is provisional too"
    );
}

#[tokio::test]
async fn a_fresh_run_after_clear_can_record_new_knowledge() {
    let h = harness(&[("src/a.rs", "fn a() {}\n")]).await;
    store(&h).clear(&repo(&h)).unwrap();
    run_calling(&h, "Inspect the retry guard", quiet_permissions(), "m1", "memory_remember",
        json!({"kind":"fact", "title":"New retry finding", "claim":"The retry guard is per order.", "applies_to":["src/a.rs"]})).await;
    assert_eq!(
        records_titled(&h, "New retry finding").len(),
        1,
        "a new run takes the current generation rather than the initial zero"
    );
}

#[tokio::test]
async fn deleting_a_memory_while_a_run_waits_removes_it_from_the_next_request() {
    let h = harness(&[("src/a.rs", "fn a() {}\n")]).await;
    let title = "zanzibar retry convention";
    let id = remember_verified(&h, title, "Serialize zanzibar retries.", &["src/a.rs"]);
    let (url, seen) = model(vec![
        Reply::Call {
            id: "q1",
            name: "ask_user",
            args: json!({"questions":[{"question":"Continue?","options":["Yes"]}]}),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(
        &url,
        Harness::send("Inspect zanzibar retries"),
        quiet_permissions(),
    ))
    .await;
    h.wait_for("questionsAsked", |e| e["type"] == "questionsAsked")
        .await;
    assert!(seen.lock().unwrap()[0].to_string().contains(title));
    store(&h).delete(id).unwrap();
    let answer =
        serde_json::from_value(json!({"answers":[{"kind":"option","label":"Yes"}]})).unwrap();
    assert!(agent_answer_questions(&h.core, h.dialog_id.clone(), "q1".into(), answer).await);
    h.run_end().await;
    assert!(
        !seen
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .to_string()
            .contains(title),
        "a removed record must not remain in the cached model packet"
    );
}

#[tokio::test]
async fn memory_remember_records_the_same_claim_once() {
    let h = harness(&[("src/a.rs", "fn a() {}\n")]).await;
    let title = "The retry table is guarded";
    let args = json!({
        "kind": "fact",
        "title": title,
        "claim": "The retry table is guarded by the zanzibar lock.",
        "applies_to": ["src/a.rs"],
    });
    let (url, _) = model(vec![
        Reply::Calls(vec![
            ("m1", "memory_remember", args.clone()),
            ("m2", "memory_remember", args.clone()),
        ]),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Tidy the notes"), quiet_permissions()))
        .await;
    h.run_end().await;

    let found = records_titled(&h, title);
    assert_eq!(
        found.len(),
        1,
        "the same claim is one memory, however often the model says it: {found:#?}"
    );
}

#[tokio::test]
async fn memory_remember_refuses_a_claim_it_cannot_write() {
    let h = harness(&[("src/a.rs", "fn a() {}\n"), ("src/ok.txt", "ok\n")]).await;
    // Settings → Restricted files: the run may not read what it would record.
    restrict(&h, json!([{ "path": "src/ok.txt" }]));

    // A blank claim, a kind the tool does not know, and a file the run may not
    // read: none of them is a memory.
    let refusals = [
        (
            "A fact with no claim",
            json!({ "kind": "fact", "title": "A fact with no claim", "claim": "   " }),
        ),
        (
            "A rumour about the queue",
            json!({
                "kind": "rumour",
                "title": "A rumour about the queue",
                "claim": "The queue is drained once a turn.",
            }),
        ),
        (
            "A fact about a file the user restricted",
            json!({
                "kind": "fact",
                "title": "A fact about a file the user restricted",
                "claim": "src/ok.txt holds the word the check wants.",
                "applies_to": ["src/ok.txt"],
                "evidence": "src/ok.txt:1 says ok",
            }),
        ),
    ];
    for (n, (title, args)) in refusals.iter().enumerate() {
        let id = ["m1", "m2", "m3"][n];
        run_calling(
            &h,
            "Tidy the notes",
            quiet_permissions(),
            id,
            "memory_remember",
            args.clone(),
        )
        .await;
        assert!(
            records_titled(&h, title).is_empty(),
            "the tool refused {title:?}, so the project's memory kept nothing of it: {:#?}",
            records_titled(&h, title)
        );
    }
    let written: Vec<String> = recent(&h).into_iter().map(|memory| memory.title).collect();
    assert!(
        written.iter().all(|title| title == "Tidy the notes"),
        "the project's memory holds the runs' own accounts and nothing the tool \
         refused: {written:?}"
    );
}

#[tokio::test]
async fn implementing_a_named_plan_saves_its_title_in_long_term_memory() {
    let plan = "# Shared backend ownership\n\nUpdate the notes.\n";
    let h = harness(&[(".rovik/plans/backend.md", plan), ("notes.txt", "before\n")]).await;
    let (url, seen) = model(vec![
        Reply::Call {
            id: "t1",
            name: "todo",
            args: json!({"items": [{"title": "Update notes", "status": "in_progress"}]}),
        },
        write("w1", "notes.txt", "after\n"),
        Reply::Call {
            id: "t2",
            name: "todo",
            args: json!({"items": [{"title": "Update notes", "status": "done"}]}),
        },
        Reply::Text("Done."),
    ])
    .await;
    let title = "Implement \"Shared backend ownership\" plan.";
    let start = StartKind::Send {
        message: UserMessage {
            id: uuid::Uuid::new_v4().to_string(),
            content: title.into(),
            meta: Some(json!({
                "mode": "agent", "implementsPlan": true,
                "planPath": ".rovik/plans/backend.md"
            })),
            attachments: Vec::new(),
        },
    };
    h.start(h.request(&url, start, quiet_permissions())).await;
    assert_eq!(h.run_end().await, "done");
    let memories = recent(&h);
    assert!(
        memories.iter().any(|memory| memory.title == title),
        "{memories:?}"
    );
    assert!(!memories
        .iter()
        .any(|memory| memory.title.starts_with("Implement this plan")));
    let episode = memories
        .iter()
        .find(|memory| memory.title == title)
        .unwrap();
    assert!(episode
        .files
        .contains(&".rovik/plans/backend.md".to_string()));
    assert!(episode
        .content
        .contains("\nPlan: .rovik/plans/backend.md\n"));
    assert!(episode.content.contains(&format!(
        "Plan fingerprint (SHA-256): {}",
        plan::plan_fingerprint(plan)
    )));
    assert!(!episode
        .content
        .contains(&h.project.to_string_lossy().to_string()));
    assert!(!episode
        .content
        .lines()
        .find(|line| line.starts_with("Changed:"))
        .unwrap()
        .contains("backend.md"));
    let file = h.project.join(".rovik").join("plans").join("backend.md");
    let shown = plan::relative_plan_path(&file.to_string_lossy(), h.project.to_str());
    let wire = plan::implementation_request(plan, Some(&shown), true);
    assert!(seen.lock().unwrap()[0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["content"].as_str() == Some(&wire)));
}
