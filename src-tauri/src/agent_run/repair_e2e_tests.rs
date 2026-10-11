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
    assert_eq!(rb["restored"], json!(["b.txt", "c.txt"]));
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
    assert!(reports[1].contains("b.txt, c.txt are as before that fix"));
    assert!(reports[1]
        .contains("Left as they are, because they changed outside this chat meanwhile: a.txt."));
    assert!(reports[1].contains("the checks run again when you finish"));
    // The failures it's sent are the ones before its fix.
    assert!(reports[1].contains("a.txt is not good"));
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
        asked[0].contains("fewer tests ran than in the last round (1 → 0)"),
        "{}",
        asked[0]
    );
    let rounds = records(&messages);
    let last = rounds.last().unwrap();
    assert_eq!(last["status"], "flagged");
    assert!(last["integrity"].as_array().unwrap().len() >= 2);
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
