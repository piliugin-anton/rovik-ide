//! Merges checked on the server (T08, 6.6, D13): when the project changed a
//! file the remote chat also changed, on other lines, the result goes back
//! with a fresh snapshot, the daemon merges it and runs the chat's checks
//! on the merge, and the new result lands. A check that fails is a fix
//! round for the chat, whose result lands afterwards. After two rounds of
//! the project changing the same files again meanwhile, the result waits.
//! Without checks to run, the merge lands here at once, as a local chat's.

mod common;

use common::app::{landed_as, App};
use common::project::{read, ready_to_run, start, unattended, writes, Setup, CHAT, PROJECT};
use common::{until, write};
use rovik_core::agent_run::permissions::Permissions;
use rovik_core::overlay::{chats, import};
use rovik_core::project_profile;
use rovik_core::testing::{model, Reply};
use serde_json::{json, Value};

const LINES: &str = "1\n2\n3\n4\n5\n";

/// The setup's project with `a.txt` and the check `command`, its app
/// connected with the project closed, and the chat's worktree ready.
async fn checked(command: &str) -> (Setup, App) {
    let mut setup = Setup::new().await;
    write(&setup.project.join("a.txt"), LINES);
    let app = App::new(&mut setup).await;
    let root = setup.project.to_string_lossy().into_owned();
    let check = json!({ "kind": "test", "command": command, "enabled": true });
    let config = &app.test.core.paths.config_dir;
    project_profile::save_project_settings(config, root, json!({ "checks": [check] })).unwrap();
    app.remote.set_open_project(None);
    app.connect(&setup).await;
    ready_to_run(&setup).await;
    (setup, app)
}

/// Its checks run when a run ends.
fn verified() -> Permissions {
    Permissions {
        verify_changes: true,
        ..unattended()
    }
}

/// The chat's result waits for the project to open; then the project
/// changes `a.txt`, on other lines than the chat, and opens.
async fn changed_here_then_opened(setup: &Setup, app: &App, lines: &str) {
    until("the result waiting for its project", || {
        chats::pending(CHAT).filter(|p| p.reason == import::READY)
    })
    .await;
    write(&setup.project.join("a.txt"), lines);
    let open = (PROJECT.to_string(), setup.project.clone());
    app.remote.set_open_project(Some(open));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_merge_checked_on_the_server_lands() {
    let (setup, app) = checked("test -f a.txt").await;
    let (url, asked) = model(vec![
        writes("w1", "a.txt", "ONE\n2\n3\n4\n5\n"),
        Reply::Text("Done."),
    ])
    .await;
    let base = setup.base();
    app.remote
        .start(start(&setup, &url, verified()))
        .await
        .unwrap();

    changed_here_then_opened(&setup, &app, "1\n2\n3\n4\nFIVE\n").await;

    landed_as(&setup, "a.txt", "ONE\n2\n3\n4\nFIVE\n").await;
    assert_ne!(setup.base(), base, "merged on the server first");
    assert_eq!(
        asked.lock().unwrap().len(),
        2,
        "the checks passed: no fix round"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_check_failing_on_the_merge_is_fixed_and_then_lands() {
    let both = "! { grep -q ONE a.txt && grep -q FIVE a.txt; }";
    let (setup, app) = checked(both).await;
    let (url, asked) = model(vec![
        writes("w1", "a.txt", "ONE\n2\n3\n4\n5\n"),
        Reply::Text("Done."),
        // The fix round on the server, after the merge.
        writes("w2", "a.txt", "ONE\n2\n3\n4\nfive\n"),
        Reply::Text("Fixed."),
    ])
    .await;
    app.remote
        .start(start(&setup, &url, verified()))
        .await
        .unwrap();

    changed_here_then_opened(&setup, &app, "1\n2\n3\n4\nFIVE\n").await;

    landed_as(&setup, "a.txt", "ONE\n2\n3\n4\nfive\n").await;
    let requests = asked.lock().unwrap();
    let told = requests
        .iter()
        .map(Value::to_string)
        .any(|r| r.contains("The project changed meanwhile (a.txt)"));
    assert!(told, "the fix round knew of the merge");
    let shown = requests
        .iter()
        .map(Value::to_string)
        .any(|r| r.contains("What the project changed in `a.txt`") && r.contains("-5\\n+FIVE"));
    assert!(shown, "and saw what the project changed");
}

#[tokio::test(flavor = "multi_thread")]
async fn after_two_rounds_of_changing_again_the_result_waits() {
    // Each run of the checks waits for the test's go: in a folder of its
    // own, as the temporary folder holds the daemon's (hidden from the
    // checks), and under Landlock a writable folder that holds a hidden one
    // has its own entries writable, but nothing new made in it.
    let gates = std::env::temp_dir().join(format!("recheck-gate-{}", std::process::id()));
    std::fs::create_dir_all(&gates).unwrap();
    let go = gates.join("go");
    let gate = format!(
        "while [ ! -f {go} ]; do sleep 0.05; done; rm -f {go}",
        go = go.display()
    );
    let (setup, app) = checked(&gate).await;
    let (url, _) = model(vec![
        writes("w1", "a.txt", "ONE\n2\n3\n4\n5\n"),
        Reply::Text("Done."),
    ])
    .await;
    write(&go, "go");
    app.remote
        .start(start(&setup, &url, verified()))
        .await
        .unwrap();
    changed_here_then_opened(&setup, &app, "1\n2\n3\n4\nFIVE\n").await;

    // Twice, the project changes again while the merge's checks run.
    let mut base = setup.base();
    for lines in ["1\n2\n3\nFOUR\nFIVE\n", "1\n2\nTHREE\nFOUR\nFIVE\n"] {
        let rechecked = until("a recheck on the server", || {
            Some(setup.base()).filter(|now| *now != base)
        })
        .await;
        base = rechecked;
        write(&setup.project.join("a.txt"), lines);
        write(&go, "go");
    }

    let waits = until("the result waiting", || {
        chats::pending(CHAT).filter(|p| p.reason.contains("again after the checks ran"))
    })
    .await;
    assert!(waits.reason.contains("a.txt"), "{}", waits.reason);
    assert_eq!(
        read(&setup.project.join("a.txt")),
        "1\n2\nTHREE\nFOUR\nFIVE\n"
    );
    let _ = std::fs::remove_dir_all(&gates);
}

#[tokio::test(flavor = "multi_thread")]
async fn without_checks_the_merge_lands_here_at_once() {
    let mut setup = Setup::new().await;
    write(&setup.project.join("a.txt"), LINES);
    let app = App::new(&mut setup).await;
    app.remote.set_open_project(None);
    app.connect(&setup).await;
    ready_to_run(&setup).await;
    let (url, _) = model(vec![
        writes("w1", "a.txt", "ONE\n2\n3\n4\n5\n"),
        Reply::Text("Done."),
    ])
    .await;
    // Checks would run at its end, but the project has none.
    app.remote
        .start(start(&setup, &url, verified()))
        .await
        .unwrap();
    let run = |setup: &Setup| setup.daemon.state()["chats"][CHAT]["runId"].clone();

    changed_here_then_opened(&setup, &app, "1\n2\n3\n4\nFIVE\n").await;
    let ran = run(&setup);

    landed_as(&setup, "a.txt", "ONE\n2\n3\n4\nFIVE\n").await;
    assert_eq!(run(&setup), ran, "no run checked the merge there");
}
