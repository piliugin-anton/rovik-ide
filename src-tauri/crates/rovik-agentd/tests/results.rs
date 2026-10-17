//! A chat's results on the real daemon (T05, 6.5): its worktree committed
//! on its landing base through the daemon's own index, without what an
//! ignore rule matches but with the base's files an ignore rule matches,
//! the agent's `git status` left as it was; the newest kept for
//! `WorkResults`, and `ResultReady` after a run's end with the reason a
//! local chat's landing would give.

mod common;

use common::project::{listening, start, unattended, Setup, CHAT};
use common::{commit_paths, write};
use git2::{
    Delta, ObjectType, Oid, Repository, Status, StatusOptions, TreeWalkMode, TreeWalkResult,
};
use rovik_client::Incoming;
use rovik_core::agent_run::{pending_reason, StartKind, UserMessage};
use rovik_core::testing::{model, model_routed, Reply};
use rovik_protocol::events::AgentEvent;
use rovik_protocol::work::ChatResult;
use rovik_protocol::{Event, Request};
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;

/// The chat's worktree, at a snapshot of the project as it is now: its
/// root, and the snapshot.
async fn worktree(setup: &Setup) -> (PathBuf, String) {
    let snapshot = setup.pushed(CHAT).await;
    setup.ensure().await;
    let root = PathBuf::from(setup.sync(CHAT, &snapshot).await.unwrap().root);
    (root, snapshot)
}

/// The files of a commit's tree, by path.
fn files(repo: &Repository, commit: &str) -> Vec<String> {
    let commit = repo.find_commit(Oid::from_str(commit).unwrap()).unwrap();
    let mut files = Vec::new();
    let tree = commit.tree().unwrap();
    tree.walk(TreeWalkMode::PreOrder, |dir, entry| {
        if entry.kind() == Some(ObjectType::Blob) {
            files.push(format!(
                "{dir}{}",
                String::from_utf8_lossy(entry.name_bytes())
            ));
        }
        TreeWalkResult::Ok
    })
    .unwrap();
    files.sort();
    files
}

/// What the agent's `git status` says in the worktree.
fn status(worktree: &Repository) -> Vec<(String, Status)> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true);
    let statuses = worktree.statuses(Some(&mut opts)).unwrap();
    let entries = statuses.iter();
    entries
        .map(|entry| (entry.path().unwrap().to_string(), entry.status()))
        .collect()
}

/// The next result the client hears of, and the outcome of the run that
/// ended before it, if one did.
async fn next_result(heard: &mut UnboundedReceiver<Incoming>) -> (ChatResult, Option<String>) {
    let mut ended = None;
    loop {
        let next = tokio::time::timeout(Duration::from_secs(30), heard.recv()).await;
        match next.expect("no result came").expect("the client went") {
            Incoming::Event {
                event: Event::ResultReady(result),
                ..
            } => return (result, ended),
            Incoming::Event {
                event: Event::Agent(AgentEvent::RunEnded { outcome, .. }),
                ..
            } => ended = Some(outcome),
            _ => {}
        }
    }
}

#[tokio::test]
async fn a_result_is_the_worktree_without_what_is_ignored() {
    let setup = Setup::new().await;
    write(&setup.project.join(".gitignore"), "target/\n*.log\n");
    // Tracked, though an ignore rule matches it.
    write(&setup.project.join("keep.log"), "kept\n");
    commit_paths(&setup.project, &[".gitignore", "keep.log"]);
    let (root, snapshot) = worktree(&setup).await;
    write(&root.join("src/main.rs"), "fn main() { changed() }\n");
    write(&root.join("src/new.rs"), "fn new() {}\n");
    write(&root.join("target/debug/out.o"), "built\n");
    write(
        &root.join("notes.mine"),
        "only the client's global excludes name it\n",
    );
    let agents = Repository::open(&root).unwrap();
    let (before, index) = (status(&agents), agents.path().join("index"));
    let index_before = std::fs::read(&index).unwrap();

    let result: ChatResult = (setup.client)
        .request(Request::WorkResult {
            dialog_id: CHAT.into(),
        })
        .await
        .unwrap();

    let there = setup.there();
    let expected = [
        ".gitignore",
        ".rovik/.gitignore",
        "keep.log",
        "src/main.rs",
        "src/new.rs",
    ];
    assert_eq!(files(&there, &result.commit), expected);
    let commit = there
        .find_commit(Oid::from_str(&result.commit).unwrap())
        .unwrap();
    assert_eq!(commit.parent_id(0).unwrap().to_string(), snapshot);
    let base = commit.parent(0).unwrap().tree().unwrap();
    let diff = (there.diff_tree_to_tree(Some(&base), Some(&commit.tree().unwrap()), None)).unwrap();
    let changes: Vec<(Delta, String)> = diff
        .deltas()
        .map(|d| {
            (
                d.status(),
                d.new_file().path().unwrap().to_string_lossy().into_owned(),
            )
        })
        .collect();
    let expected = [
        (Delta::Modified, "src/main.rs".to_string()),
        (Delta::Added, "src/new.rs".to_string()),
    ];
    assert_eq!(changes, expected, "nothing of the base reads as deleted");
    // The agent's git status didn't change under it.
    assert_eq!(status(&agents), before);
    assert_eq!(std::fs::read(&index).unwrap(), index_before);
    // No run ended: the result waits as a local chat's would.
    assert_eq!(result.outcome, "unknown");
    assert_eq!(result.pending, pending_reason("unknown"));

    let results: Vec<ChatResult> = setup.client.request(Request::WorkResults).await.unwrap();
    assert_eq!(results, [result.clone()]);
    let again: ChatResult = (setup.client)
        .request(Request::WorkResult {
            dialog_id: CHAT.into(),
        })
        .await
        .unwrap();
    assert_eq!(
        again.commit, result.commit,
        "nothing changed, so the same commit"
    );
}

#[tokio::test]
async fn a_runs_end_brings_its_result() {
    let setup = Setup::new().await;
    let (root, _) = worktree(&setup).await;
    let (_listener, mut heard) = listening(&setup.daemon).await;
    let (url, _) = model(vec![
        Reply::Call {
            id: "w1",
            name: "write",
            args: json!({ "path": "src/new.rs", "content": "fn new() {}\n" }),
        },
        Reply::Text("Done."),
        // The second run: no reply, until it's stopped.
        Reply::Hang(None),
    ])
    .await;

    let first = start(&setup, &url, unattended());
    setup
        .client
        .request::<Option<String>>(Request::AgentStart(first))
        .await
        .unwrap();
    let (result, outcome) = next_result(&mut heard).await;

    let outcome = outcome.expect("the run's end comes first");
    assert_eq!(result.outcome, outcome);
    assert_eq!(result.pending, pending_reason(&outcome));
    assert!(files(&setup.there(), &result.commit).contains(&"src/new.rs".to_string()));
    assert!(root.join("src/new.rs").exists());

    let mut second = start(&setup, &url, unattended());
    second.start = rovik_core::agent_run::StartKind::Send {
        message: rovik_core::agent_run::UserMessage {
            id: "u2".into(),
            content: "More".into(),
            meta: None,
            attachments: Vec::new(),
        },
    };
    setup
        .client
        .request::<Option<String>>(Request::AgentStart(second))
        .await
        .unwrap();
    let stop = Request::AgentStop {
        dialog_id: CHAT.into(),
    };
    assert!(setup.client.request::<bool>(stop).await.unwrap());
    let (stopped, outcome) = next_result(&mut heard).await;

    assert_eq!(outcome.as_deref(), Some("stopped"));
    assert_eq!(
        stopped.pending.as_deref(),
        Some("Not landed: the run was stopped.")
    );
    assert_eq!(
        stopped.commit, result.commit,
        "the worktree is as the first run left it"
    );
}

/// V02 (D7): an Autopilot task's implementation on the server, in a
/// project with no checks and a plan with nothing to run: its result
/// doesn't land on the computer on its own, and says why. Its plan's run,
/// which changed nothing, brings an ordinary result first.
#[tokio::test]
async fn an_autopilot_tasks_unchecked_result_waits() {
    let setup = Setup::new().await;
    let (root, _) = worktree(&setup).await;
    let (_listener, mut heard) = listening(&setup.daemon).await;
    let plan = "# Say hello\n\nmain prints hello.\n\n## Steps\n\n\
        1. **Say hello**\n   - Files: `src/main.rs`\n   - Change `main` in `src/main.rs` to print `hello` with `println!`.\n\
        2. **Check**\n   - Files: `src/main.rs` (read)\n   - Read `src/main.rs` to confirm `main` prints `hello`; it keeps its signature.\n\n\
        ## Validation\n- Run it by hand: it prints hello.\n";
    let (url, _) = model_routed(
        vec![("# Review this plan", vec![Reply::Text("## Findings\n\nNone.")])],
        vec![
            Reply::Call {
                id: "p1",
                name: "submit_plan",
                args: json!({ "plan": plan }),
            },
            Reply::Text("Saved the plan."),
            Reply::Call {
                id: "t1",
                name: "todo",
                args: json!({ "items": [
                    { "title": "Say hello", "status": "in_progress" },
                    { "title": "Check", "status": "pending" },
                ] }),
            },
            Reply::Call {
                id: "w1",
                name: "write",
                args: json!({ "path": "src/main.rs", "content": "fn main() { println!(\"hello\"); }\n" }),
            },
            Reply::Call {
                id: "t2",
                name: "todo",
                args: json!({ "items": [
                    { "title": "Say hello", "status": "done" },
                    { "title": "Check", "status": "done" },
                ] }),
            },
            Reply::Text("Done: main says hello."),
        ],
    )
    .await;
    let mut request = start(&setup, &url, unattended());
    request.start = StartKind::Send {
        message: UserMessage {
            id: "u1".into(),
            content: "Make main say hello".into(),
            meta: Some(json!({ "mode": "autopilot" })),
            attachments: Vec::new(),
        },
    };
    request.mode = Some("autopilot".into());
    setup
        .client
        .request::<Option<String>>(Request::AgentStart(request))
        .await
        .unwrap();
    let (planned, outcome) = next_result(&mut heard).await;
    assert_eq!(outcome.as_deref(), Some("done"), "the plan's run");
    assert_eq!(planned.pending, None);
    let (implemented, outcome) = next_result(&mut heard).await;
    assert_eq!(outcome.as_deref(), Some("unverified"));
    assert_eq!(
        implemented.pending.as_deref(),
        Some("Not landed: no check ran on it, and Autopilot lands only what's checked.")
    );
    assert!(std::fs::read_to_string(root.join("src/main.rs"))
        .unwrap()
        .contains("hello"));
}
