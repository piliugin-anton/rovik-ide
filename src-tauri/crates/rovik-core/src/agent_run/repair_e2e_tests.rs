//! The verification gate's fix rounds (repair.rs) in whole runs: progress
//! between rounds (R01), rollback of a fix that broke something (R02), and
//! tests weakened to pass (R05). The project's test check is a node script.

use super::*;

/// A test runner in miniature: `test(name, fn)` cases from tests/*.test.js,
/// reported as cargo does ("test t::name ... FAILED", then a summary).
const RUNNER: &str = r#"
const fs = require('fs');
const cases = [];
global.test = (name, fn) => cases.push({ name, fn });
global.test.skip = () => {};
global.assert = (ok, msg) => { if (!ok) throw new Error(msg || 'assertion failed'); };
for (const f of fs.readdirSync('tests').filter((f) => f.endsWith('.test.js')).sort()) require('./tests/' + f);
let failed = 0;
for (const c of cases) {
  try { c.fn(); console.log('test t::' + c.name + ' ... ok'); }
  catch (e) { failed++; console.log('test t::' + c.name + ' ... FAILED'); console.log('  ' + e.message); }
}
console.log('test result: ' + (failed ? 'FAILED' : 'ok') + '. ' + (cases.length - failed) + ' passed; ' + failed + ' failed');
process.exit(failed ? 1 : 0);
"#;

const PACKAGE: &str = r#"{ "name": "t", "scripts": { "test": "node run.js" } }"#;

/// Two tests: `one` reads a.txt, `two` reads b.txt.
const TESTS: &str = r#"
const fs = require('fs');
test('one', () => assert(fs.readFileSync('a.txt', 'utf8').includes('good'), 'a.txt is not good'));
test('two', () => assert(!fs.readFileSync('b.txt', 'utf8').includes('broken'), 'b.txt is broken'));
"#;

fn has_node() -> bool {
    std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_ok()
}

fn write(id: &'static str, path: &str, content: &str) -> Reply {
    Reply::Call {
        id,
        name: "write",
        args: json!({ "path": path, "content": content }),
    }
}

fn records(messages: &[MessageRow]) -> Vec<Value> {
    messages
        .iter()
        .filter_map(|m| m.meta.as_ref()?.get("verification").cloned())
        .collect()
}

fn harness_texts(messages: &[MessageRow], kind: &str) -> Vec<String> {
    messages
        .iter()
        .filter(|m| m.meta.as_ref().is_some_and(|m| m["harness"] == kind))
        .map(|m| m.content.clone())
        .collect()
}

fn checks_on() -> Permissions {
    Permissions {
        allow_any_command: true,
        checklist: ChecklistMode::Off,
        ..Permissions::default()
    }
}

#[tokio::test]
async fn the_same_failures_twice_is_no_progress() {
    if !has_node() {
        return;
    }
    let h = harness(&[
        ("package.json", PACKAGE),
        ("run.js", RUNNER),
        ("tests/a.test.js", TESTS),
        ("a.txt", "bad\n"),
        ("b.txt", "fine\n"),
    ])
    .await;
    let (url, _) = model(vec![
        write("w1", "notes.txt", "first try\n"),
        Reply::Text("Done."),
        // Round 2: a "fix" that changes nothing that matters.
        write("w2", "notes.txt", "second try\n"),
        Reply::Text("Done."),
        // Round 3: the real fix.
        write("w3", "a.txt", "good\n"),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Fix the tests"), checks_on()))
        .await;
    assert_eq!(h.run_end().await, "verified");
    let messages = h.messages().await;
    let rounds = records(&messages);
    let statuses: Vec<&Value> = rounds.iter().map(|r| &r["status"]).collect();
    assert_eq!(
        statuses,
        [&json!("failed"), &json!("failed"), &json!("passed")]
    );
    assert!(
        rounds[0].get("progress").is_none(),
        "nothing to compare with"
    );
    assert_eq!(
        rounds[0]["checks"][0]["failures"],
        json!(["test|test|t::one"])
    );
    assert_eq!(rounds[0]["testsRun"], 2);
    assert_eq!(rounds[1]["progress"]["verdict"], "no-progress");
    assert_eq!(rounds[2]["progress"]["verdict"], "progress");

    let reports = harness_texts(&messages, "verification");
    assert_eq!(reports.len(), 2);
    assert!(!reports[0].contains("No progress"));
    assert!(
        reports[1].contains("No progress: your last fix left exactly the same failures."),
        "{}",
        reports[1]
    );
}

#[tokio::test]
async fn a_fix_that_breaks_a_passing_test_is_rolled_back() {
    if !has_node() {
        return;
    }
    let h = harness(&[
        ("package.json", PACKAGE),
        ("run.js", RUNNER),
        ("tests/a.test.js", TESTS),
        ("a.txt", "bad\n"),
        ("b.txt", "fine\n"),
    ])
    .await;
    let (url, _) = model(vec![
        write("w1", "notes.txt", "looking\n"),
        write("w0", "d.txt", "keep me\n"),
        Reply::Text("Done."),
        // Round 2 fixes `one` and breaks `two`; a.txt is then changed by a
        // command (as if by someone else): not this run's write to undo.
        write("w2", "a.txt", "good\n"),
        write("w3", "b.txt", "broken\n"),
        write("w4", "c.txt", "new file\n"),
        Reply::Call {
            id: "b1",
            name: "bash",
            args: json!({ "command": "printf 'good, by hand\\n' > a.txt" }),
        },
        // P3.4: and deletes a file it wrote before the round.
        Reply::Call {
            id: "b2",
            name: "bash",
            args: json!({ "command": "rm d.txt" }),
        },
        Reply::Text("Done."),
        // Told what broke and what was left as it was, it looks, sees a.txt
        // is fine, and finishes: the checks run again anyway.
        Reply::Call {
            id: "r1",
            name: "read",
            args: json!({ "path": "a.txt" }),
        },
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Fix the tests"), checks_on()))
        .await;
    assert_eq!(h.run_end().await, "verified");

    // b.txt and the new c.txt went back; a.txt, changed outside the run's
    // own writes, was left alone.
    assert_eq!(h.file("b.txt").as_deref(), Some("fine\n"));
    assert_eq!(h.file("c.txt"), None);
    assert_eq!(h.file("a.txt").as_deref(), Some("good, by hand\n"));

    let messages = h.messages().await;
    let rounds = records(&messages);
    let statuses: Vec<&Value> = rounds.iter().map(|r| &r["status"]).collect();
    assert_eq!(
        statuses,
        [&json!("failed"), &json!("rolled-back"), &json!("passed")]
    );
    let rb = &rounds[1]["rolledBack"];
    assert_eq!(rb["broke"], json!(["test|test|t::two"]));
    assert_eq!(rb["restored"], json!(["b.txt", "c.txt", "d.txt"]));
    // P3.4: the deleted file is back, and the editor heard c.txt went.
    assert_eq!(h.file("d.txt").as_deref(), Some("keep me\n"));
    assert!(
        h.events_of("fileWritten")
            .iter()
            .any(|e| e["deleted"] == true && e["absPath"].as_str().unwrap().ends_with("c.txt")),
        "{:?}",
        h.events_of("fileWritten")
    );
    assert_eq!(rb["skipped"], json!(["a.txt"]));
    assert!(rb["diff"].as_str().unwrap().contains("broken"));
    assert_eq!(rounds[1]["progress"]["verdict"], "regression");
    assert_eq!(rounds[2]["progress"]["verdict"], "progress");

    let reports = harness_texts(&messages, "verification");
    assert!(
        reports[1]
            .contains("Your last fix broke something that didn't fail before:\n- test|t::two"),
        "{}",
        reports[1]
    );
    assert!(reports[1].contains("b.txt, c.txt, d.txt are as before that fix"));
    // P3.5: the commands of the fix weren't undone, and it says so.
    assert_eq!(
        rb["commands"],
        json!(["printf 'good, by hand\\n' > a.txt", "rm d.txt"])
    );
    assert!(
        reports[1].contains("Commands ran in that fix (`printf 'good, by hand\\n' > a.txt`, `rm d.txt`); what they changed wasn't undone"),
        "{}",
        reports[1]
    );
    assert!(reports[1]
        .contains("Left as they are, because they changed outside this chat meanwhile: a.txt."));
    assert!(reports[1].contains("the checks run again when you finish"));
    // The failures it's sent are the ones before its fix.
    assert!(reports[1].contains("a.txt is not good"));
}

fn bash(id: &'static str, command: &str) -> Reply {
    Reply::Call {
        id,
        name: "bash",
        args: json!({ "command": command }),
    }
}

/// Round 2 (§4.2), working in the project: a rolled-back round's created
/// file goes, its deleted one comes back, its modified one has its old
/// bytes. A file the round wrote and then deleted, and one the user
/// created again where the round deleted it, are left as they are.
/// A failed check's saved output is its whole output, as the reports say:
/// a line from the middle of a long one is in it, not only the start and
/// the end the report shows.
#[tokio::test]
async fn a_failed_checks_saved_output_is_the_whole_of_it() {
    if !has_node() {
        return;
    }
    let big = r#"
const filler = 'x'.repeat(120);
for (let i = 0; i < 1500; i++) console.log('before ' + i + ' ' + filler);
console.log('THE-MIDDLE-LINE');
for (let i = 0; i < 1500; i++) console.log('after ' + i + ' ' + filler);
// Not process.exit(): it cuts off what's still being written to the pipe.
process.exitCode = 1;
"#;
    let h = harness(&[
        (
            "package.json",
            r#"{ "name": "t", "scripts": { "test": "node big.js" } }"#,
        ),
        ("big.js", big),
    ])
    .await;
    let (url, _) = model(vec![
        write("w1", "notes.txt", "a change\n"),
        Reply::Text("Done."),
        Reply::Text("I'll leave it."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Go"), checks_on()))
        .await;
    h.run_end().await;
    let rounds = records(&h.messages().await);
    let saved = rounds[0]["checks"][0]["savedOutput"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(saved.starts_with("@checks/"), "{}", rounds[0]);
    let file = crate::agent_run::check_output::resolve(&h.dialog_id, &saved).unwrap();
    let text = std::fs::read_to_string(file).unwrap();
    assert!(text.contains("THE-MIDDLE-LINE"));
    let start: String = text.chars().take(400).collect();
    assert!(
        text.contains("before 0 ") && text.contains("after 1499 "),
        "{start}"
    );
}

#[tokio::test]
async fn rollback_of_create_delete_modify_direct() {
    if !has_node() {
        return;
    }
    let h = harness(&[
        ("package.json", PACKAGE),
        ("run.js", RUNNER),
        ("tests/a.test.js", TESTS),
        ("a.txt", "bad\n"),
        ("b.txt", "fine\n"),
        ("m.txt", "old\n"),
        ("f.txt", "f before\n"),
    ])
    .await;
    let (url, _) = model(vec![
        // Before any checks: files the run then deletes in its fix.
        write("w0", "d.txt", "keep me\n"),
        write("w1", "u.txt", "the run's\n"),
        Reply::Text("Done."),
        // The fix: breaks `two`, and creates, deletes, modifies.
        write("w2", "b.txt", "broken\n"),
        write("w3", "c.txt", "new file\n"),
        write("w4", "m.txt", "new\n"),
        write("w5", "f.txt", "f written\n"),
        bash("b1", "rm d.txt f.txt u.txt"),
        Reply::Slow(1500, Box::new(Reply::Text("Done."))),
        // Rolled back, it fixes `one`.
        write("w6", "a.txt", "good\n"),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Fix the tests"), checks_on()))
        .await;
    // The user creates u.txt again after the round deleted it.
    let u = h.project.join("u.txt");
    for _ in 0..500 {
        if h.file("c.txt").is_some() && !u.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!u.exists(), "the round deleted it");
    std::fs::write(&u, "the user's\n").unwrap();
    assert_eq!(h.run_end().await, "verified");

    let rounds = records(&h.messages().await);
    let rb = &rounds[1]["rolledBack"];
    assert_eq!(rounds[1]["status"], "rolled-back", "{}", rounds[1]);
    assert_eq!(rb["restored"], json!(["b.txt", "c.txt", "d.txt", "m.txt"]));
    assert_eq!(rb["skipped"], json!(["f.txt", "u.txt"]));
    assert_eq!(h.file("c.txt"), None);
    assert_eq!(h.file("d.txt").as_deref(), Some("keep me\n"));
    assert_eq!(h.file("m.txt").as_deref(), Some("old\n"));
    assert_eq!(h.file("b.txt").as_deref(), Some("fine\n"));
    assert_eq!(h.file("f.txt"), None, "deleted by the round, not restored");
    assert_eq!(h.file("u.txt").as_deref(), Some("the user's\n"));

    let written = h.events_of("fileWritten");
    let event = |name: &str| {
        written
            .iter()
            .rev()
            .find(|e| e["absPath"].as_str().unwrap().ends_with(name))
            .cloned()
            .unwrap()
    };
    assert_eq!(event("c.txt")["deleted"], true);
    let d = event("d.txt");
    assert_eq!(d["before"], Value::Null, "{d}");
    assert_eq!(d["content"], "keep me\n");
    assert!(d.get("deleted").is_none(), "{d}");
    assert_eq!(event("m.txt")["before"], "new\n");
}

/// Round 2 (§4.2): a rollback lists the commands of the round it undoes:
/// not those before it, nor read-only ones.
#[tokio::test]
async fn commands_are_counted_per_round() {
    if !has_node() {
        return;
    }
    let h = harness(&[
        ("package.json", PACKAGE),
        ("run.js", RUNNER),
        ("tests/a.test.js", TESTS),
        ("a.txt", "bad\n"),
        ("b.txt", "fine\n"),
    ])
    .await;
    let (url, _) = model(vec![
        // Before the first checks: a command that writes.
        bash("b1", "printf 'looking\\n' > notes.txt"),
        Reply::Text("Done."),
        // The fix: reads with commands, and breaks `two`.
        bash("b2", "cat a.txt"),
        bash("b3", "env FOO=1 grep bad a.txt"),
        write("w1", "b.txt", "broken\n"),
        Reply::Text("Done."),
        write("w2", "a.txt", "good\n"),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Fix the tests"), checks_on()))
        .await;
    let end = h.run_end().await;
    let rounds = records(&h.messages().await);
    assert_eq!(end, "verified", "{rounds:#?}");
    let statuses: Vec<&Value> = rounds.iter().map(|r| &r["status"]).collect();
    assert_eq!(
        statuses,
        [&json!("failed"), &json!("rolled-back"), &json!("passed")]
    );
    let rb = &rounds[1]["rolledBack"];
    assert_eq!(rb["restored"], json!(["b.txt"]), "{rb}");
    assert!(rb.get("commands").is_none(), "{rb}");
}

/// Round 2: with command approval on, a fix round's checks run in a new
/// run; the commands the fix ran are still listed if it's rolled back.
#[tokio::test]
async fn a_rounds_commands_survive_waiting_for_approval() {
    if !has_node() {
        return;
    }
    let h = harness(&[
        ("package.json", PACKAGE),
        ("run.js", RUNNER),
        ("tests/a.test.js", TESTS),
        ("a.txt", "bad\n"),
        ("b.txt", "fine\n"),
    ])
    .await;
    let (url, _) = model(vec![
        write("w0", "notes.txt", "looking\n"),
        Reply::Text("Done."),
        // After "Run checks": the fix runs a command and breaks `two`.
        bash("b1", "printf 'more\\n' >> notes.txt"),
        write("w1", "b.txt", "broken\n"),
        Reply::Text("Done."),
        write("w2", "a.txt", "good\n"),
        Reply::Text("Done."),
    ])
    .await;
    let permissions = Permissions {
        command_approval: permissions::ApprovalRule {
            required: true,
            by: permissions::Approver::User,
        },
        ..checks_on()
    };
    h.start(h.request(&url, Harness::send("Fix the tests"), permissions.clone()))
        .await;
    let mut outcome = h.run_end().await;
    let mut runs = 0;
    while outcome == "verify-needs-approval" && runs < 4 {
        runs += 1;
        let record = h.messages().await.last().unwrap().id.clone();
        h.events.lock().unwrap().clear();
        h.start(h.request(
            &url,
            StartKind::RunChecks { record_id: record },
            permissions.clone(),
        ))
        .await;
        if runs == 1 {
            // The fix's command waits for approval too.
            h.wait_for("approval b1", |e| {
                e["type"] == "approvalRequested" && e["request"]["toolCallId"] == "b1"
            })
            .await;
            let core = h.core.clone();
            agent_resolve_approval(
                &core,
                h.dialog_id.clone(),
                "b1".into(),
                ApprovalDecision::Allow,
            )
            .await;
        }
        outcome = h.run_end().await;
    }
    let rounds = records(&h.messages().await);
    assert_eq!(outcome, "verified", "{rounds:#?}");
    let rb = rounds
        .iter()
        .find(|r| r["status"] == "rolled-back")
        .unwrap_or_else(|| panic!("{rounds:#?}"));
    assert_eq!(
        rb["rolledBack"]["commands"],
        json!(["printf 'more\\n' >> notes.txt"]),
        "{rb}"
    );
}

/// Round 2 (§4.2), in an isolated chat: a rolled-back round's created file
/// goes, its deleted one comes back, its modified one has its old bytes,
/// whether the write tool or a command changed them; no commands are
/// listed (the chat's copy undoes theirs too). Needs FUSE.
#[tokio::test]
async fn rollback_of_create_delete_modify_isolated() {
    if !has_node() || !crate::vfs::mount_works() {
        return;
    }
    let mut h = harness(&[
        ("package.json", PACKAGE),
        ("run.js", RUNNER),
        ("tests/a.test.js", TESTS),
        ("a.txt", "bad\n"),
        ("b.txt", "fine\n"),
        ("m.txt", "old\n"),
        ("d.txt", "keep me\n"),
        ("e.txt", "keep me too\n"),
    ])
    .await;
    h.isolate().await;
    let (url, _) = model(vec![
        write("w0", "notes.txt", "looking\n"),
        Reply::Text("Done."),
        // The fix: breaks `two`, and creates, deletes, modifies, with the
        // write tool and with a command.
        write("w1", "b.txt", "broken\n"),
        write("w2", "c.txt", "new file\n"),
        write("w3", "m.txt", "new\n"),
        bash(
            "b1",
            "rm d.txt && printf 'made\\n' > n.txt && printf 'changed\\n' > e.txt",
        ),
        Reply::Text("Done."),
        write("w4", "a.txt", "good\n"),
        Reply::Text("Done."),
    ])
    .await;
    let mut request = h.request(&url, Harness::send("Fix the tests"), checks_on());
    request.isolate = Some(true);
    h.start(request).await;
    let end = h.run_end().await;
    let rounds = records(&h.messages().await);
    assert_eq!(end, "verified", "{rounds:#?}");
    assert_eq!(rounds[1]["status"], "rolled-back", "{}", rounds[1]);
    let rb = &rounds[1]["rolledBack"];
    assert_eq!(
        rb["restored"],
        json!(["b.txt", "c.txt", "d.txt", "e.txt", "m.txt", "n.txt"]),
        "{rb}"
    );
    assert!(rb.get("skipped").is_none_or(|s| s == &json!([])), "{rb}");
    assert!(rb.get("commands").is_none(), "{rb}");
    assert!(
        h.events_of("fileWritten")
            .iter()
            .any(|e| e["deleted"] == true && e["absPath"].as_str().unwrap().ends_with("c.txt")),
        "{:?}",
        h.events_of("fileWritten")
    );
    // Verified, so it landed: only the last fix is in the project.
    assert_eq!(h.file("a.txt").as_deref(), Some("good\n"));
    assert_eq!(h.file("b.txt").as_deref(), Some("fine\n"));
    assert_eq!(h.file("c.txt"), None);
    assert_eq!(h.file("d.txt").as_deref(), Some("keep me\n"));
    assert_eq!(h.file("e.txt").as_deref(), Some("keep me too\n"));
    assert_eq!(h.file("m.txt").as_deref(), Some("old\n"));
    assert_eq!(h.file("n.txt"), None);
}

/// A fix round's write to a file outside the project is refused, and it
/// isn't put in the round's checkpoint either: rolled back, the round
/// doesn't write that file back over what the user has there since.
#[tokio::test]
async fn a_rollback_never_writes_a_file_outside_the_project() {
    if !has_node() || !crate::vfs::mount_works() {
        return;
    }
    let mut h = harness(&[
        ("package.json", PACKAGE),
        ("run.js", RUNNER),
        ("tests/a.test.js", TESTS),
        ("a.txt", "bad\n"),
        ("b.txt", "fine\n"),
    ])
    .await;
    h.isolate().await;
    let outside = h
        .project
        .parent()
        .unwrap()
        .join(format!("outside-{}.txt", std::process::id()));
    std::fs::write(&outside, "before\n").unwrap();
    let outside_path: &'static str =
        Box::leak(outside.to_string_lossy().into_owned().into_boxed_str());
    let (url, _) = model(vec![
        write("w0", "notes.txt", "looking\n"),
        Reply::Text("Done."),
        // The fix: a write outside the project, then one that breaks `two`.
        write("w1", outside_path, "the chat's\n"),
        write("w2", "b.txt", "broken\n"),
        Reply::Slow(1500, Box::new(Reply::Text("Done."))),
        write("w3", "a.txt", "good\n"),
        Reply::Text("Done."),
    ])
    .await;
    let mut request = h.request(&url, Harness::send("Fix the tests"), checks_on());
    request.isolate = Some(true);
    h.start(request).await;
    // The user changes the file while the round is still going.
    for _ in 0..500 {
        let written = h.events_of("fileWritten");
        if written
            .iter()
            .any(|e| e["absPath"].as_str().is_some_and(|p| p.ends_with("b.txt")))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    std::fs::write(&outside, "the user's\n").unwrap();
    let end = h.run_end().await;
    let rounds = records(&h.messages().await);
    assert_eq!(end, "verified", "{rounds:#?}");
    assert_eq!(rounds[1]["status"], "rolled-back", "{}", rounds[1]);
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "the user's\n");
    let _ = std::fs::remove_file(&outside);
}

/// Round 2 (§4.2), in an isolated chat: after a restart, what each file
/// was is rebuilt from the project's copy (its hash still matches), so a
/// test weakened before the wait is still seen.
#[tokio::test]
async fn rebuild_from_hashes_isolated_matches_project_copy() {
    if !has_node() || !crate::vfs::mount_works() {
        return;
    }
    let tests = r#"test('adds', () => assert(require('../add.js')(1, 2) === 3, 'add is wrong'));
"#;
    let mut h = harness(&[
        ("package.json", PACKAGE),
        ("run.js", RUNNER),
        ("tests/add.test.js", tests),
        ("add.js", "module.exports = (a, b) => a - b;\n"),
    ])
    .await;
    h.isolate().await;
    let skipped: &'static str = Box::leak(tests.replace("test(", "test.skip(").into_boxed_str());
    let tests: &'static str = Box::leak(tests.to_string().into_boxed_str());
    let (url, _) = model(vec![
        write("w1", "tests/add.test.js", skipped),
        Reply::Text("Done."),
        // Asked: puts the test back and fixes the code instead.
        write("w2", "tests/add.test.js", tests),
        write("w3", "add.js", "module.exports = (a, b) => a + b;\n"),
        Reply::Text("Fixed properly."),
    ])
    .await;
    let permissions = Permissions {
        command_approval: permissions::ApprovalRule {
            required: true,
            by: permissions::Approver::User,
        },
        ..checks_on()
    };
    let mut request = h.request(&url, Harness::send("Fix add"), permissions.clone());
    request.isolate = Some(true);
    h.start(request).await;
    assert_eq!(h.run_end().await, "verify-needs-approval");
    let waiting = h.messages().await.last().unwrap().id.clone();
    // The restart: what the run carried is gone.
    h.core.agents.drop_carry(&h.dialog_id);
    h.events.lock().unwrap().clear();
    let mut request = h.request(
        &url,
        StartKind::RunChecks {
            record_id: waiting.clone(),
        },
        permissions.clone(),
    );
    request.isolate = Some(true);
    h.start(request).await;
    let mut outcome = h.run_end().await;
    // The fix's checks wait for approval too.
    if outcome == "verify-needs-approval" {
        let next = h.messages().await.last().unwrap().id.clone();
        h.events.lock().unwrap().clear();
        let mut request = h.request(&url, StartKind::RunChecks { record_id: next }, permissions);
        request.isolate = Some(true);
        h.start(request).await;
        outcome = h.run_end().await;
    }
    let messages = h.messages().await;
    let v = verification_of(&messages, &waiting);
    assert_eq!(outcome, "verified", "{v}");
    assert_eq!(v["stateRestored"], false, "{v}");
    assert!(v.get("integrityChecked").is_none(), "rebuilt: {v}");
    assert!(
        !harness_texts(&messages, "integrity").is_empty(),
        "the weakened test was seen"
    );
    assert_eq!(h.file("tests/add.test.js").as_deref(), Some(tests));
}

/// Round 2 (§4.2): a round whose check timed out says nothing was
/// resolved: what it didn't get to isn't fixed.
#[tokio::test]
async fn a_timeout_round_doesnt_say_resolved_e2e() {
    if !has_node() {
        return;
    }
    let h = harness(&[
        ("package.json", r#"{ "name": "t", "scripts": { "test": "node check.js" } }"#),
        (
            "check.js",
            "const fs = require('fs'); const a = fs.readFileSync('a.txt', 'utf8'); \
             if (a.includes('slow')) { setInterval(() => {}, 1000); } \
             else if (!a.includes('ok')) { console.error('a.txt:1: error: not ok'); process.exit(1); }",
        ),
        ("a.txt", "no\n"),
    ])
    .await;
    verify::CHECK_TIMEOUTS
        .lock()
        .unwrap()
        .insert(h.dialog_id.clone(), 3000);
    let (url, _) = model(vec![
        write("w1", "a.txt", "nope\n"),
        Reply::Text("Done."),
        write("w2", "a.txt", "slow\n"),
        Reply::Text("Fixed."),
        write("w3", "a.txt", "ok\n"),
        Reply::Text("Fixed."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Fix a"), checks_on()))
        .await;
    let end = h.run_end().await;
    verify::CHECK_TIMEOUTS.lock().unwrap().remove(&h.dialog_id);
    let messages = h.messages().await;
    let rounds = records(&messages);
    assert_eq!(end, "verified", "{rounds:#?}");
    assert_eq!(rounds[1]["checks"][0]["timedOut"], true, "{}", rounds[1]);
    assert_ne!(
        rounds[1]["progress"]["verdict"], "progress",
        "{}",
        rounds[1]
    );
    assert!(
        rounds[1]["progress"]["fixed"]
            .as_array()
            .is_none_or(Vec::is_empty),
        "{}",
        rounds[1]
    );
    let reports = harness_texts(&messages, "verification");
    assert!(!reports[1].contains("resolved"), "{}", reports[1]);
}

#[tokio::test]
async fn a_test_skipped_to_pass_is_not_reported_as_verified() {
    if !has_node() {
        return;
    }
    let tests = r#"test('adds', () => assert(require('../add.js')(1, 2) === 3, 'add is wrong'));
"#;
    let h = harness(&[
        ("package.json", PACKAGE),
        ("run.js", RUNNER),
        ("tests/add.test.js", tests),
        ("add.js", "module.exports = (a, b) => a - b;\n"),
    ])
    .await;
    let skipped = tests.replace("test(", "test.skip(");
    let skipped: &'static str = Box::leak(skipped.into_boxed_str());
    let (url, _) = model(vec![
        write("w1", "add.js", "module.exports = (a, b) => a * b;\n"),
        Reply::Text("Done."),
        // It can't fix it, so it silences the test.
        write("w2", "tests/add.test.js", skipped),
        Reply::Text("Done."),
        // Asked about it, it keeps it.
        Reply::Text("The test was wrong, so I skipped it."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Fix add"), checks_on()))
        .await;
    assert_eq!(h.run_end().await, "verify-flagged");

    let messages = h.messages().await;
    let asked = harness_texts(&messages, "integrity");
    assert_eq!(asked.len(), 1);
    assert!(
        asked[0].contains("tests/add.test.js: skip markers added (0 → 1)"),
        "{}",
        asked[0]
    );
    assert!(
        asked[0]
            .contains("fewer tests ran than before, beyond any the run removed from its own test files (1 → 0)"),
        "{}",
        asked[0]
    );
    let rounds = records(&messages);
    let last = rounds.last().unwrap();
    assert_eq!(last["status"], "flagged");
    assert!(last["integrity"].as_array().unwrap().len() >= 2);
}

/// P3.7: the run adds two tests of its own, then removes one: fewer tests
/// than the round before, but not fewer than before it touched any: no flag.
#[tokio::test]
async fn removing_a_test_the_run_added_isnt_flagged() {
    if !has_node() {
        return;
    }
    let tests = r#"test('adds', () => assert(require('../add.js')(1, 2) === 3, 'add is wrong'));
"#;
    let h = harness(&[
        ("package.json", PACKAGE),
        ("run.js", RUNNER),
        ("tests/add.test.js", tests),
        ("add.js", "module.exports = (a, b) => a - b;\n"),
    ])
    .await;
    let two = "test('zero', () => assert(require('../add.js')(0, 0) === 0));\ntest('one', () => assert(require('../add.js')(1, 0) === 1));\n";
    let one = "test('zero', () => assert(require('../add.js')(0, 0) === 0));\n";
    let (url, _) = model(vec![
        write("w1", "tests/extra.test.js", two),
        Reply::Text("Done."),
        write("w2", "add.js", "module.exports = (a, b) => a + b;\n"),
        write("w3", "tests/extra.test.js", one),
        Reply::Text("Fixed, and dropped a redundant test of mine."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Fix add"), checks_on()))
        .await;
    assert_eq!(h.run_end().await, "verified");
    let rounds = records(&h.messages().await);
    assert_eq!(rounds.last().unwrap()["testsRun"], 2);
    assert!(rounds.last().unwrap().get("integrity").is_none());
}

/// P3.9: a test weakened before the checks waited for approval, then the
/// app restarted (the carried state gone): the checks pass, but the run
/// isn't verified; the record says integrity wasn't checked.
#[tokio::test]
async fn after_a_restart_lost_state_is_never_verified() {
    if !has_node() {
        return;
    }
    let tests = r#"test('adds', () => assert(require('../add.js')(1, 2) === 3, 'add is wrong'));
"#;
    let h = harness(&[
        ("package.json", PACKAGE),
        ("run.js", RUNNER),
        ("tests/add.test.js", tests),
        ("add.js", "module.exports = (a, b) => a - b;\n"),
    ])
    .await;
    let skipped: &'static str = Box::leak(tests.replace("test(", "test.skip(").into_boxed_str());
    let (url, _) = model(vec![
        write("w1", "tests/add.test.js", skipped),
        Reply::Text("Done."),
    ])
    .await;
    let permissions = Permissions {
        command_approval: permissions::ApprovalRule {
            required: true,
            by: permissions::Approver::User,
        },
        ..checks_on()
    };
    h.start(h.request(&url, Harness::send("Fix add"), permissions.clone()))
        .await;
    assert_eq!(h.run_end().await, "verify-needs-approval");
    let waiting = h.messages().await.last().unwrap().id.clone();
    // The restart: what the run carried is gone.
    h.core.agents.drop_carry(&h.dialog_id);
    h.events.lock().unwrap().clear();
    h.start(h.request(
        &url,
        StartKind::RunChecks {
            record_id: waiting.clone(),
        },
        permissions,
    ))
    .await;
    assert_eq!(h.run_end().await, "verify-unchecked");
    let record = h
        .messages()
        .await
        .into_iter()
        .find(|m| m.id == waiting)
        .unwrap();
    let v = &record.meta.unwrap()["verification"];
    assert_eq!(v["status"], "passed");
    assert_eq!(v["integrityChecked"], false, "{v}");
}

#[tokio::test]
async fn undoing_a_flagged_change_verifies_the_run() {
    if !has_node() {
        return;
    }
    let tests = r#"test('adds', () => assert(require('../add.js')(1, 2) === 3, 'add is wrong'));
"#;
    let h = harness(&[
        ("package.json", PACKAGE),
        ("run.js", RUNNER),
        ("tests/add.test.js", tests),
        ("add.js", "module.exports = (a, b) => a - b;\n"),
    ])
    .await;
    let skipped: &'static str = Box::leak(tests.replace("test(", "test.skip(").into_boxed_str());
    let tests: &'static str = Box::leak(tests.to_string().into_boxed_str());
    let (url, _) = model(vec![
        write("w1", "tests/add.test.js", skipped),
        Reply::Text("Done."),
        // Asked: puts the test back and fixes the code instead.
        write("w2", "tests/add.test.js", tests),
        write("w3", "add.js", "module.exports = (a, b) => a + b;\n"),
        Reply::Text("Fixed properly."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Fix add"), checks_on()))
        .await;
    assert_eq!(h.run_end().await, "verified");
    let rounds = records(&h.messages().await);
    assert_eq!(rounds.last().unwrap()["status"], "passed");
    assert!(rounds.last().unwrap().get("integrity").is_none());
}

/// V05: three checks, each on its own file: lint on users.txt, the build
/// on posts.txt, and the tests, whose `tags` test reads tags.txt.
fn three_checks() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "package.json",
            r#"{ "name": "t", "scripts": { "lint": "node lint.js", "build": "node build.js", "test": "node run.js" } }"#,
        ),
        (
            "lint.js",
            "if (require('fs').readFileSync('users.txt', 'utf8').includes('bad')) { console.log('users.txt:1: error: users is bad'); process.exit(1); }",
        ),
        (
            "build.js",
            "if (require('fs').readFileSync('posts.txt', 'utf8').includes('bad')) { console.log('posts.txt:1: error: posts is bad'); process.exit(1); }",
        ),
        ("run.js", RUNNER),
        (
            "tests/tags.test.js",
            "const fs = require('fs');\ntest('tags', () => assert(!fs.readFileSync('tags.txt', 'utf8').includes('broken'), 'tags.txt is broken'));\n",
        ),
        ("users.txt", "bad\n"),
        ("posts.txt", "bad\n"),
        ("tags.txt", "fine\n"),
    ]
}

/// V05: a round fixes two checks and breaks the tests' `tags`: only
/// tags.txt, the file behind it, goes back; the two fixes stay, and the
/// checks run again at once and pass.
#[tokio::test]
async fn a_fix_that_breaks_one_check_keeps_its_other_fixes() {
    if !has_node() {
        return;
    }
    let h = harness(&three_checks()).await;
    let (url, _) = model(vec![
        write("w1", "notes.txt", "looking\n"),
        Reply::Text("Done."),
        // Fixes lint and the build; breaks `tags`.
        write("w2", "users.txt", "good\n"),
        write("w3", "posts.txt", "good\n"),
        write("w4", "tags.txt", "broken\n"),
        Reply::Text("Done."),
        // Never asked: the checks pass once tags.txt is back.
        Reply::Text("Unexpected."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Fix the checks"), checks_on()))
        .await;
    assert_eq!(h.run_end().await, "verified");
    assert_eq!(h.file("users.txt").as_deref(), Some("good\n"), "kept");
    assert_eq!(h.file("posts.txt").as_deref(), Some("good\n"), "kept");
    assert_eq!(h.file("tags.txt").as_deref(), Some("fine\n"), "put back");
    let messages = h.messages().await;
    let rounds = records(&messages);
    let statuses: Vec<&Value> = rounds.iter().map(|r| &r["status"]).collect();
    assert_eq!(
        statuses,
        [&json!("failed"), &json!("rolled-back"), &json!("passed")]
    );
    let rb = &rounds[1]["rolledBack"];
    assert_eq!(rb["broke"], json!(["test|test|t::tags"]));
    assert_eq!(rb["restored"], json!(["tags.txt"]));
    assert_eq!(rb["kept"], json!(["posts.txt", "users.txt"]));
    // The round counts once: the checks of what stayed are round 2's.
    assert_eq!(rounds[2]["round"], 2);
    // The agent was told of round 1 only: the rest needed nobody.
    assert_eq!(harness_texts(&messages, "verification").len(), 1);
    assert!(!messages.iter().any(|m| m.content == "Unexpected."));
}

/// V05: the files behind what broke went back, and the rest of the fix
/// still breaks it: then everything goes back, as before V05, and the
/// agent has the round it would have had.
#[tokio::test]
async fn when_the_rest_still_breaks_it_everything_goes_back() {
    if !has_node() {
        return;
    }
    let mut files = three_checks();
    // `tags` reads posts.txt too: the build's fix breaks it as well.
    files[4] = (
        "tests/tags.test.js",
        "const fs = require('fs');\ntest('tags', () => assert(!fs.readFileSync('tags.txt', 'utf8').includes('broken') && !fs.readFileSync('posts.txt', 'utf8').includes('good'), 'tags is broken'));\n",
    );
    let h = harness(&files).await;
    let (url, _) = model(vec![
        write("w1", "notes.txt", "looking\n"),
        Reply::Text("Done."),
        write("w2", "users.txt", "good\n"),
        write("w3", "posts.txt", "good\n"),
        write("w4", "tags.txt", "broken\n"),
        Reply::Text("Done."),
        // Told everything went back, it stops there.
        Reply::Text("I'll leave it."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Fix the checks"), checks_on()))
        .await;
    // Four rounds of three checks each: a busy runner takes past 15 s.
    h.run_end_within(Duration::from_secs(60)).await;
    for (file, was) in [
        ("users.txt", "bad\n"),
        ("posts.txt", "bad\n"),
        ("tags.txt", "fine\n"),
    ] {
        assert_eq!(
            h.file(file).as_deref(),
            Some(was),
            "{file}: as before round 2"
        );
    }
    let messages = h.messages().await;
    let rounds = records(&messages);
    let statuses: Vec<&Value> = rounds.iter().map(|r| &r["status"]).collect();
    assert_eq!(
        statuses,
        [
            &json!("failed"),
            &json!("rolled-back"),
            &json!("rolled-back"),
            &json!("unresolved")
        ],
        "{rounds:#?}"
    );
    assert_eq!(
        rounds[1]["rolledBack"]["kept"],
        json!(["posts.txt", "users.txt"])
    );
    let all = &rounds[2]["rolledBack"];
    assert!(all.get("kept").is_none(), "everything: {all}");
    assert_eq!(all["restored"], json!(["posts.txt", "users.txt"]));
    assert_eq!(rounds[2]["round"], 2, "the same round, counted once");
    // The agent hears it as attempt 2: the third is still its own.
    let reports = harness_texts(&messages, "verification");
    assert_eq!(reports.len(), 2);
    assert!(reports[1].contains("(attempt 2 of 3)"), "{}", reports[1]);
    assert!(reports[1].contains("users is bad"), "{}", reports[1]);
}

#[tokio::test]
async fn a_fix_that_lets_the_tests_run_is_not_rolled_back() {
    if !has_node() {
        return;
    }
    // The build fails until build.txt says ok; then the tests run.
    let runner: &'static str = Box::leak(
        format!(
            "if (!require('fs').readFileSync('build.txt', 'utf8').includes('ok')) {{\n\
               console.log('error[E0425]: cannot find value `x`\\n --> src/lib.rs:1:5');\n\
               process.exit(1);\n}}\n{RUNNER}"
        )
        .into_boxed_str(),
    );
    let h = harness(&[
        ("package.json", PACKAGE),
        ("run.js", runner),
        ("tests/a.test.js", TESTS),
        ("build.txt", "fails\n"),
        ("a.txt", "bad\n"),
        ("b.txt", "fine\n"),
    ])
    .await;
    let (url, _) = model(vec![
        write("w1", "notes.txt", "looking\n"),
        Reply::Text("Done."),
        // Fixes the build: now test `one` runs, and fails.
        write("w2", "build.txt", "ok\n"),
        Reply::Text("Done."),
        write("w3", "a.txt", "good\n"),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Fix it"), checks_on()))
        .await;
    assert_eq!(h.run_end().await, "verified");
    assert_eq!(h.file("build.txt").as_deref(), Some("ok\n"), "kept");
    let messages = h.messages().await;
    let rounds = records(&messages);
    let statuses: Vec<&Value> = rounds.iter().map(|r| &r["status"]).collect();
    assert_eq!(
        statuses,
        [&json!("failed"), &json!("failed"), &json!("passed")]
    );
    assert_eq!(rounds[1]["progress"]["verdict"], "progress");
    let reports = harness_texts(&messages, "verification");
    assert!(
        reports[1].contains("may have hidden") && reports[1].contains("a.txt is not good"),
        "{}",
        reports[1]
    );
}

#[tokio::test]
async fn putting_back_a_weakened_test_that_fails_is_not_rolled_back() {
    if !has_node() {
        return;
    }
    let tests = r#"test('adds', () => assert(require('../add.js')(1, 2) === 3, 'add is wrong'));
test('runs', () => assert(true));
"#;
    let h = harness(&[
        ("package.json", PACKAGE),
        ("run.js", RUNNER),
        ("tests/add.test.js", tests),
        ("add.js", "module.exports = (a, b) => a - b;\n"),
    ])
    .await;
    let weakened: &'static str = Box::leak(
        tests
            .replace("test('adds'", "test.skip('adds'")
            .into_boxed_str(),
    );
    let tests: &'static str = Box::leak(tests.to_string().into_boxed_str());
    let (url, _) = model(vec![
        // It silences the failing test: the checks pass, and it's asked.
        write("w1", "tests/add.test.js", weakened),
        Reply::Text("Done."),
        // It puts the test back, as asked: the test fails again.
        write("w2", "tests/add.test.js", tests),
        Reply::Text("Put it back."),
        // Then it fixes the code.
        write("w3", "add.js", "module.exports = (a, b) => a + b;\n"),
        Reply::Text("Fixed."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Fix add"), checks_on()))
        .await;
    assert_eq!(h.run_end().await, "verified");
    // The test it put back stays put back.
    assert_eq!(h.file("tests/add.test.js").as_deref(), Some(tests));
    let messages = h.messages().await;
    let rounds = records(&messages);
    let statuses: Vec<&Value> = rounds.iter().map(|r| &r["status"]).collect();
    assert_eq!(
        statuses,
        [&json!("passed"), &json!("failed"), &json!("passed")]
    );
    let reports = harness_texts(&messages, "verification");
    assert_eq!(reports.len(), 1);
    assert!(reports[0].contains("add is wrong"), "{}", reports[0]);
    assert!(!reports[0].contains("broke something"));
    assert!(rounds.last().unwrap().get("integrity").is_none());
}

/// With command approval on, each round of checks is a run of its own: the
/// fix rounds' state goes from one to the next, so a fix that broke a
/// passing test is still rolled back.
#[tokio::test]
async fn with_approval_on_a_fix_that_breaks_a_test_is_still_rolled_back() {
    if !has_node() {
        return;
    }
    let h = harness(&[
        ("package.json", PACKAGE),
        ("run.js", RUNNER),
        ("tests/a.test.js", TESTS),
        ("a.txt", "bad\n"),
        ("b.txt", "fine\n"),
    ])
    .await;
    let (url, _) = model(vec![
        write("w1", "notes.txt", "looking\n"),
        Reply::Text("Done."),
        // Round 1 fails; its fix fixes `one` and breaks `two`.
        write("w2", "a.txt", "good\n"),
        write("w3", "b.txt", "broken\n"),
        Reply::Text("Done."),
        // Told it was rolled back, it stops.
        Reply::Text("I'll leave it."),
    ])
    .await;
    let permissions = Permissions {
        allow_any_command: true,
        checklist: ChecklistMode::Off,
        command_approval: permissions::ApprovalRule {
            required: true,
            by: permissions::Approver::User,
        },
        ..Permissions::default()
    };
    h.start(h.request(&url, Harness::send("Fix the tests"), permissions.clone()))
        .await;
    assert_eq!(h.run_end().await, "verify-needs-approval");
    for _ in 0..2 {
        let waiting = h
            .messages()
            .await
            .into_iter()
            .rev()
            .find(|m| {
                m.meta
                    .as_ref()
                    .is_some_and(|m| m["verification"]["status"] == "needs-approval")
            })
            .expect("checks waiting");
        h.events.lock().unwrap().clear();
        h.start(h.request(
            &url,
            StartKind::RunChecks {
                record_id: waiting.id.clone(),
            },
            permissions.clone(),
        ))
        .await;
        h.run_end().await;
    }
    // The rounds that ran (the next one waits for approval again).
    let rounds: Vec<Value> = records(&h.messages().await)
        .into_iter()
        .filter(|r| r["status"] != "needs-approval")
        .collect();
    let statuses: Vec<&Value> = rounds.iter().map(|r| &r["status"]).collect();
    assert_eq!(
        statuses[..2],
        [&json!("failed"), &json!("rolled-back")],
        "{rounds:#?}"
    );
    assert_eq!(
        rounds[1]["rolledBack"]["broke"],
        json!(["test|test|t::two"])
    );
    assert_eq!(h.file("b.txt").as_deref(), Some("fine\n"));
}

/// Files too big to keep a checkpoint of: a fix that breaks a passing test
/// can't be undone. The round says so, and the agent hears what broke.
#[tokio::test]
async fn a_regression_past_the_checkpoint_limit_is_reported_not_rolled_back() {
    if !has_node() {
        return;
    }
    let h = harness(&[
        ("package.json", PACKAGE),
        ("run.js", RUNNER),
        ("tests/a.test.js", TESTS),
        ("a.txt", "bad\n"),
        ("b.txt", "fine\n"),
    ])
    .await;
    *crate::agent_run::repair::CHECKPOINT_LIMIT_IN
        .lock()
        .unwrap() = Some((h.project.to_string_lossy().into_owned(), 1));
    let (url, _) = model(vec![
        write("w1", "notes.txt", "looking\n"),
        Reply::Text("Done."),
        write("w2", "a.txt", "good\n"),
        write("w3", "b.txt", "broken\n"),
        Reply::Text("Done."),
        write("w4", "b.txt", "fine\n"),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Fix the tests"), checks_on()))
        .await;
    let end = h.run_end().await;
    *crate::agent_run::repair::CHECKPOINT_LIMIT_IN
        .lock()
        .unwrap() = None;
    assert_eq!(end, "verified");
    assert_eq!(h.file("b.txt").as_deref(), Some("fine\n"));
    let messages = h.messages().await;
    let rounds = records(&messages);
    let statuses: Vec<&Value> = rounds.iter().map(|r| &r["status"]).collect();
    assert_eq!(
        statuses,
        [&json!("failed"), &json!("failed"), &json!("passed")]
    );
    assert_eq!(rounds[1]["progress"]["verdict"], "regression");
    assert!(rounds[1]["rolledBack"].is_null(), "{:#}", rounds[1]);
    assert_eq!(
        rounds[1]["notRolledBack"],
        "the files it changed were too large to keep a copy of"
    );
    let reports = harness_texts(&messages, "verification");
    assert!(reports[1].contains("t::two"), "{}", reports[1]);
    assert!(
        reports[1].contains("couldn't be undone"),
        "the agent hears it broke something and that it stays: {}",
        reports[1]
    );
}

/// Round 2, P3.2: a lint error the fix itself left in the file it wrote,
/// printed with an absolute path as linters do, is reported with the
/// failures; the fix stays (no rollback).
#[tokio::test]
async fn lint_slip_e2e_no_rollback() {
    if !has_node() {
        return;
    }
    const LINT: &str = r#"
const fs = require('fs');
if (fs.readFileSync('a.js', 'utf8').includes('require')) {
  console.log(require('path').resolve('a.js') + ":1:7: error: 'path' is assigned a value but never used");
  process.exit(1);
}
"#;
    let h = harness(&[
        (
            "package.json",
            r#"{ "name": "t", "scripts": { "lint": "node lint.js", "test": "node run.js" } }"#,
        ),
        ("run.js", RUNNER),
        ("lint.js", LINT),
        ("tests/a.test.js", TESTS),
        ("a.txt", "bad\n"),
        ("b.txt", "fine\n"),
        ("a.js", "module.exports = 1;\n"),
    ])
    .await;
    let (url, _) = model(vec![
        write("w1", "notes.txt", "looking\n"),
        Reply::Text("Done."),
        // Round 2 fixes the test, and leaves an unused import behind.
        write("w2", "a.txt", "good\n"),
        write(
            "w3",
            "a.js",
            "const path = require('path');\nmodule.exports = 1;\n",
        ),
        Reply::Text("Done."),
        // Round 3: the slip, tidied.
        write("w4", "a.js", "module.exports = 1;\n"),
        Reply::Text("Done."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Fix the tests"), checks_on()))
        .await;
    assert_eq!(h.run_end().await, "verified");
    let rounds = records(&h.messages().await);
    let statuses: Vec<&Value> = rounds.iter().map(|r| &r["status"]).collect();
    assert_eq!(
        statuses,
        [&json!("failed"), &json!("failed"), &json!("passed")],
        "{rounds:#?}"
    );
    assert_eq!(
        rounds[1]["progress"]["verdict"], "progress",
        "{:#}",
        rounds[1]
    );
    assert!(rounds[1]["rolledBack"].is_null(), "{:#}", rounds[1]);
    assert_eq!(
        h.file("a.txt").as_deref(),
        Some("good\n"),
        "the fix is kept"
    );
}

/// Two failing tests of add.js, for the P3.7 runs below.
const ADD_TESTS: &str = r#"test('adds', () => assert(require('../add.js')(1, 2) === 3, 'add is wrong'));
test('adds zero', () => assert(require('../add.js')(2, 0) === 2, 'add is wrong'));
"#;
const NEW_TEST: &str = "test('new', () => assert(true));\n";

fn rm_add_tests(id: &'static str) -> Reply {
    Reply::Call {
        id,
        name: "bash",
        args: json!({ "command": "rm tests/add.test.js" }),
    }
}

async fn fewer_tests_run(lead: Vec<Reply>) -> (Harness, String) {
    let h = harness(&[
        ("package.json", PACKAGE),
        ("run.js", RUNNER),
        ("tests/add.test.js", ADD_TESTS),
        ("add.js", "module.exports = (a, b) => a * b;\n"),
    ])
    .await;
    let (url, _) = model(lead).await;
    h.start(h.request(&url, Harness::send("Fix add"), checks_on()))
        .await;
    let end = h.run_end().await;
    (h, end)
}

/// Round 2, P3.7: a test file deleted by a command, after the run wrote a
/// test file of its own: fewer tests is still flagged (direct mode, where
/// the command's deletion isn't one of the run's files).
#[tokio::test]
async fn fewer_tests_after_a_bash_deletion_is_flagged_in_direct_mode() {
    if !has_node() {
        return;
    }
    let (h, end) = fewer_tests_run(vec![
        write("w1", "add.js", "module.exports = (a, b) => a - b;\n"),
        Reply::Text("Done."),
        write("w2", "tests/new.test.js", NEW_TEST),
        rm_add_tests("b1"),
        Reply::Text("Done."),
        Reply::Text("Those tests were obsolete."),
    ])
    .await;
    assert_eq!(end, "verify-flagged");
    let asked = harness_texts(&h.messages().await, "integrity");
    assert!(
        asked[0]
            .contains("fewer tests ran than before, beyond any the run removed from its own test files (2 → 1)"),
        "{asked:?}"
    );
}

/// Round 2, P3.7: the run's first write is a test file, so no round ran
/// before it touched tests; a later deletion is still measured against
/// the rounds before.
#[tokio::test]
async fn first_write_is_a_test_file_keeps_a_reference() {
    if !has_node() {
        return;
    }
    let (h, end) = fewer_tests_run(vec![
        write("w1", "tests/new.test.js", NEW_TEST),
        Reply::Text("Done."),
        rm_add_tests("b1"),
        Reply::Text("Done."),
        Reply::Text("Those tests were obsolete."),
    ])
    .await;
    let rounds = records(&h.messages().await);
    assert_eq!(rounds[0]["testsRun"], 3, "{rounds:#?}");
    assert_eq!(rounds[1]["testsRun"], 1, "{rounds:#?}");
    assert_eq!(end, "verify-flagged");
}

/// R05: a suppression the run adds of what the checks report (an
/// `eslint-disable` in plain code): the checks pass, the agent is asked
/// once, and keeping it leaves the run flagged, not verified; taking it
/// out leaves it verified.
#[tokio::test]
async fn a_new_suppression_is_asked_about_and_flagged_if_kept() {
    if !has_node() {
        return;
    }
    let files = [
        ("package.json", PACKAGE),
        ("run.js", RUNNER),
        ("tests/ok.test.js", "test('ok', () => assert(true));\n"),
        ("add.js", "module.exports = (a, b) => a + b;\n"),
    ];
    let silenced = "// eslint-disable-next-line no-unused-vars\nconst unused = 1;\nmodule.exports = (a, b) => a + b;\n";
    let h = harness(&files).await;
    let (url, _) = model(vec![
        write("w1", "add.js", silenced),
        Reply::Text("Done."),
        // Asked about it, it keeps it.
        Reply::Text("The lint rule is wrong here."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Tidy add"), checks_on()))
        .await;
    assert_eq!(h.run_end().await, "verify-flagged");
    let messages = h.messages().await;
    let asked = harness_texts(&messages, "integrity");
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(
        asked[0].contains(
            "add.js: adds suppressions that hide what the checks report (`eslint-disable` 0 → 1)"
        ),
        "{}",
        asked[0]
    );
    assert!(
        asked[0].contains("what a check reports is fixed in the code, not silenced"),
        "{}",
        asked[0]
    );
    assert_eq!(records(&messages).last().unwrap()["status"], "flagged");

    // Taken out once asked: nothing weakens the checks, and it's verified.
    let h = harness(&files).await;
    let (url, _) = model(vec![
        write("w1", "add.js", silenced),
        Reply::Text("Done."),
        write(
            "w2",
            "add.js",
            "// Adds two numbers.\nmodule.exports = (a, b) => a + b;\n",
        ),
        Reply::Text("Took the suppression out."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Tidy add"), checks_on()))
        .await;
    assert_eq!(h.run_end().await, "verified");
}

/// Round 2 (§4.2): a test file in a language the IDE can't read the tests
/// of (a Ruby spec), changed by the run: the checks pass, the agent is
/// asked once, and keeping the change leaves the run flagged, not verified.
#[tokio::test]
async fn integrity_e2e_unsupported_language() {
    if !has_node() {
        return;
    }
    let h = harness(&[
        ("package.json", PACKAGE),
        ("run.js", RUNNER),
        ("tests/ok.test.js", "test('ok', () => assert(true));\n"),
        (
            "spec/a_spec.rb",
            "it 'adds' do\n  expect(add(1, 2)).to eq(3)\nend\n",
        ),
    ])
    .await;
    let (url, _) = model(vec![
        write("w1", "spec/a_spec.rb", "it 'adds' do\nend\n"),
        Reply::Text("Done."),
        // Asked about it, it keeps it.
        Reply::Text("The spec had to change."),
    ])
    .await;
    h.start(h.request(&url, Harness::send("Tidy the spec"), checks_on()))
        .await;
    assert_eq!(h.run_end().await, "verify-flagged");
    let messages = h.messages().await;
    let asked = harness_texts(&messages, "integrity");
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(
        asked[0].contains("spec/a_spec.rb: a test file the IDE can't check was changed"),
        "{}",
        asked[0]
    );
    let rounds = records(&messages);
    let last = rounds.last().unwrap();
    assert_eq!(last["status"], "flagged", "{last}");
    assert!(
        last["integrity"].as_array().is_some_and(|flags| flags
            .iter()
            .any(|f| f.as_str().is_some_and(|f| f.starts_with("spec/a_spec.rb:")))),
        "{last}"
    );
}
