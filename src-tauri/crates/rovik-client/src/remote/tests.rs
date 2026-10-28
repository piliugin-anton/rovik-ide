//! The app's remote chats (P08): events as 6.1 passes them on, requests
//! routed to the owner, and what a remote start sends.

use super::*;
use crate::testing::{identity, Fake};
use rovik_core::agent_run::events::Patch;
use rovik_core::agent_run::permissions::{ApprovalRule, Permissions};
use rovik_core::agent_run::{ModelProfile, RunningRun, UserMessage};
use rovik_core::chat_db::store::{MessagePatch, MessageRow, NewDialog};
use rovik_core::testing::{temp_dir, TempDir};
use rovik_core::{EventSink, Paths};
use rovik_protocol::events::EditProposal;
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

mod pages;

const HOST: &str = "test-server";

/// The app's core: the events it sends the frontend, and for each row a
/// `messagesAdded` announced, whether its database had it by then.
struct App {
    core: Core,
    events: Arc<Mutex<Vec<Value>>>,
    witnessed: Arc<Mutex<Vec<(String, bool)>>>,
    dir: TempDir,
}

struct Witness {
    database: PathBuf,
    events: Arc<Mutex<Vec<Value>>>,
    witnessed: Arc<Mutex<Vec<(String, bool)>>>,
}

impl EventSink for Witness {
    fn agent(&self, event: &AgentEvent) {
        if let AgentEvent::MessagesAdded { messages, .. } = event {
            let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY;
            let db = rusqlite::Connection::open_with_flags(&self.database, flags).unwrap();
            for m in messages {
                let found = db
                    .query_row("SELECT 1 FROM messages WHERE id = ?1", [&m.id], |_| Ok(()))
                    .optional()
                    .unwrap()
                    .is_some();
                self.witnessed.lock().unwrap().push((m.id.clone(), found));
            }
        }
        let event = serde_json::to_value(event).unwrap();
        self.events.lock().unwrap().push(event);
    }
}

impl App {
    fn new() -> App {
        let dir = temp_dir();
        let (events, witnessed) = (Arc::default(), Arc::default());
        let sink = Witness {
            database: dir.0.join("data/chats.db"),
            events: Arc::clone(&events),
            witnessed: Arc::clone(&witnessed),
        };
        let paths = Paths::new(dir.0.join("data"), dir.0.join("config"));
        let core = Core::open(paths, Arc::new(sink)).unwrap();
        App {
            core,
            events,
            witnessed,
            dir,
        }
    }

    /// A chat with these messages, in `project` if any.
    async fn chat(&self, id: &str, project: Option<&str>, messages: Vec<NewMessage>) {
        let dialog = NewDialog {
            id: id.into(),
            title: "Chat".into(),
            created_at: 0,
            updated_at: 0,
            project_id: project.map(str::to_string),
        };
        let id = id.to_string();
        self.core
            .db
            .write(move |c| {
                store::create_dialog(c, &dialog)?;
                store::append_messages(c, &id, &messages).map(|_| ())
            })
            .await
            .unwrap();
    }

    async fn rows(&self, dialog_id: &str) -> Vec<MessageRow> {
        let id = dialog_id.to_string();
        self.core
            .db
            .read(move |c| store::get_messages(c, &id, None, 100))
            .await
            .unwrap()
            .messages
    }

    fn sent(&self) -> Vec<Value> {
        self.events.lock().unwrap().clone()
    }
}

fn message(id: &str, role: &str, content: &str) -> NewMessage {
    NewMessage {
        id: id.into(),
        role: role.into(),
        content: content.into(),
        tool_calls: None,
        tool_call_id: None,
        meta: None,
        error: None,
        failed: false,
        attachments: Vec::new(),
        created_at: 1,
        finalized: true,
    }
}

fn event(event: AgentEvent) -> Incoming {
    Incoming::Event {
        seq: 1,
        event: Event::Agent(event),
    }
}

fn approval(dialog_id: &str, edit: bool) -> ApprovalRequest {
    ApprovalRequest {
        tool_call_id: if edit { "w1" } else { "c1" }.into(),
        dialog_id: dialog_id.into(),
        tool_name: if edit { "write" } else { "bash" }.into(),
        summary: "A change".into(),
        command: None,
        note: None,
        edit: edit.then(|| EditProposal {
            abs_path: "/srv/work/a.txt".into(),
            rel_path: "a.txt".into(),
            current: "a\n".into(),
            proposed: "b\n".into(),
        }),
        parent_dialog_id: None,
        parent_call_id: None,
        remote: false,
    }
}

/// A remote of `app` with `Fake` as `HOST`, connected, once it has caught
/// up as a first connection does (its `AgentStatus`): the fake keeps the
/// requests after that.
async fn connected(app: &App) -> (Arc<Remote>, Fake) {
    connected_to(app, Fake::default()).await
}

/// `connected`, with `fake` as `HOST`.
async fn connected_to(app: &App, fake: Fake) -> (Arc<Remote>, Fake) {
    let remote = Remote::new(app.core.clone());
    remote.connect(HOST, fake.clone(), identity(HOST));
    for _ in 0..400 {
        let caught_up = fake
            .requests
            .lock()
            .unwrap()
            .contains(&Request::AgentStatus);
        if caught_up {
            fake.requests.lock().unwrap().clear();
            return (remote, fake);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the fake daemon never answered");
}

fn starts(fake: &Fake) -> Vec<StartRequest> {
    let requests = fake.requests.lock().unwrap();
    requests
        .iter()
        .filter_map(|r| match r {
            Request::AgentStart(start) => Some((**start).clone()),
            _ => None,
        })
        .collect()
}

fn start(dialog_id: &str, kind: StartKind, current: Option<&str>) -> StartRequest {
    StartRequest {
        dialog_id: dialog_id.into(),
        start: kind,
        profile: ModelProfile {
            id: "p::m".into(),
            label: "m".into(),
            model: "m".into(),
            base_url: "http://model.example/v1".into(),
            api_key: "sk-1".into(),
            stream: true,
            vision: false,
            pdf: false,
            context_tokens: None,
            no_tools: false,
            first_byte_timeout_ms: None,
            api_route: None,
            connector: None,
        },
        options: None,
        mode: Some("agent".into()),
        context_mode: Some("trimming".into()),
        root: Some("/home/me/project".into()),
        current_user_content: current.map(str::to_string),
        local_date: None,
        config: None,
        isolate: None,
        project_settings: None,
        mcp_secrets: Default::default(),
    }
}

#[tokio::test]
async fn events_reach_the_frontend_as_6_1_says() {
    let app = App::new();
    app.chat("chat", None, Vec::new()).await;
    let remote = Remote::new(app.core.clone());
    remote.place("chat", HOST);
    let mut reply = message("a1", "assistant", "");
    reply.finalized = false;
    let added = AgentEvent::MessagesAdded {
        dialog_id: "chat".into(),
        messages: vec![events::out_message("chat", 7, &reply, true)],
    };
    remote.receive(HOST, event(added)).await;
    let patched = AgentEvent::MessagePatched {
        dialog_id: "chat".into(),
        message_id: "a1".into(),
        patch: Patch {
            content: Some("Done.".into()),
            finalized: Some(true),
            ..Patch::default()
        },
    };
    remote.receive(HOST, event(patched)).await;
    let written = AgentEvent::FileWritten {
        dialog_id: Some("chat".into()),
        abs_path: "/srv/work/a.txt".into(),
        before: None,
        content: "b\n".into(),
        reviewed: false,
        deleted: false,
    };
    remote.receive(HOST, event(written)).await;
    let notice = AgentEvent::Notice {
        dialog_id: Some("chat".into()),
        kind: "warning".into(),
        title: "Stopped".into(),
        message: "Why".into(),
    };
    remote.receive(HOST, event(notice)).await;
    for edit in [true, false] {
        let request = approval("chat", edit);
        remote
            .receive(HOST, event(AgentEvent::ApprovalRequested { request }))
            .await;
    }
    // Another computer's chat, which the daemon wouldn't send: dropped.
    let elsewhere = AgentEvent::RunStarted {
        dialog_id: "elsewhere".into(),
        run_id: "r".into(),
        steerable: false,
    };
    remote.receive(HOST, event(elsewhere)).await;

    // The row was in the mirror before the frontend heard of it, at its
    // owner's number. Its patches come with the next pull (6.3).
    assert_eq!(*app.witnessed.lock().unwrap(), [("a1".to_string(), true)]);
    let rows = app.rows("chat").await;
    assert_eq!(rows[0].seq, 7);
    assert!(
        !rows[0].finalized,
        "a patch isn't written: a pull brings it"
    );
    let sent = app.sent();
    let kinds: Vec<&str> = sent.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        [
            "messagesAdded",
            "messagePatched",
            "notice",
            "approvalRequested",
            "approvalRequested"
        ]
    );
    assert_eq!(sent[0]["messages"][0]["seq"], 7, "the owner's numbers");
    assert_eq!(sent[2]["title"], "test-server: Stopped");
    assert_eq!(sent[3]["request"]["remote"], true, "an edit");
    assert!(sent[4]["request"].get("remote").is_none(), "a command");
}

/// A chat the database puts on the host, which nothing placed here yet (a
/// subagent's chat made while this computer was away, brought by a pull):
/// its events are passed on, and it's routed from then on; one on no host
/// is dropped as before.
#[tokio::test]
async fn a_chat_the_database_puts_on_the_host_is_placed_by_its_first_event() {
    let app = App::new();
    app.chat("sub", None, Vec::new()).await;
    app.chat("elsewhere", None, Vec::new()).await;
    app.core
        .db
        .write(|c| location::set(c, "sub", &Location::Host(HOST.into())))
        .await
        .unwrap();
    let remote = Remote::new(app.core.clone());
    assert_eq!(remote.host_of("sub"), None);
    for chat in ["sub", "elsewhere"] {
        let started = AgentEvent::RunStarted {
            dialog_id: chat.into(),
            run_id: "r".into(),
            steerable: false,
        };
        remote.receive(HOST, event(started)).await;
    }
    assert_eq!(remote.host_of("sub").as_deref(), Some(HOST));
    assert_eq!(remote.host_of("elsewhere"), None);
    let sent = app.sent();
    let chats: Vec<&str> = sent
        .iter()
        .filter(|e| e["type"] == "runStarted")
        .map(|e| e["dialogId"].as_str().unwrap())
        .collect();
    assert_eq!(chats, ["sub"]);
}

/// 6.1: a request keyed by a message goes to its chat's owner, found from
/// the message's row here; a local chat's stays here.
#[tokio::test]
async fn a_messages_update_goes_to_its_chats_owner() {
    let app = App::new();
    app.chat("remote-chat", None, vec![message("m1", "user", "Hi")])
        .await;
    app.chat("local-chat", None, vec![message("m2", "user", "Hi")])
        .await;
    let (remote, fake) = connected(&app).await;
    remote.place("remote-chat", HOST);
    let failed = |id: &str| Request::ChatUpdateMessage {
        message_id: id.into(),
        patch: MessagePatch {
            failed: Some(true),
            ..MessagePatch::default()
        },
    };
    let reply = remote.route_message("m1", failed("m1")).await;
    assert_eq!(reply, Ok(Response::Done));
    let reply = remote.route_message("m2", failed("m2")).await;
    assert_eq!(reply, Ok(Response::Done));
    let requests: Vec<Request> = fake.requests.lock().unwrap().clone();
    // The mirror's pull of the chat placed there comes when it comes.
    let requests: Vec<Request> = requests
        .into_iter()
        .filter(|r| !matches!(r, Request::ChatChangesSince { .. }))
        .collect();
    assert_eq!(requests, [failed("m1")], "only the remote chat's");
    // The owner has it, and so does the mirror.
    assert!(app.rows("remote-chat").await[0].failed);
    assert!(app.rows("local-chat").await[0].failed);
}

/// K08, K03, 6.9: a remote start names files from the project's root, and
/// carries the project's settings and the host's approval policy.
#[tokio::test]
async fn a_remote_start_is_prepared_for_its_host() {
    let app = App::new();
    app.chat("chat", None, Vec::new()).await;
    let settings = json!({ "checks": [{ "kind": "test", "command": "cargo test" }] });
    project_profile::save_project_settings(
        &app.core.paths.config_dir,
        "/home/me/project".into(),
        settings.clone(),
    )
    .unwrap();
    let (remote, fake) = connected(&app).await;
    remote.place("chat", HOST);
    remote.set_policy(HOST, ApprovalPolicy::AiReviewerThenMe);
    let sent = "[Active editor context: /home/me/project/src/x.rs]\n\nFix it";
    let send = StartKind::Send {
        message: UserMessage {
            id: "u1".into(),
            content: "Fix it".into(),
            meta: Some(json!({ "sentContent": sent })),
            attachments: Vec::new(),
        },
    };
    let mut request = start("chat", send, Some(sent));
    let mut config = AgentConfig::default();
    config.permissions.edit_approval = ApprovalRule {
        required: true,
        by: Approver::User,
    };
    config.approval_profiles = vec![request.profile.clone()];
    request.config = Some(config);
    let reply = remote.start(request).await;
    assert_eq!(reply, Ok(Response::RunId(Some("run-1".into()))));

    let started = starts(&fake).remove(0);
    let relative = "[Active editor context: src/x.rs]\n\nFix it";
    assert_eq!(started.current_user_content.as_deref(), Some(relative));
    let StartKind::Send { message } = &started.start else {
        panic!("{started:?}");
    };
    assert_eq!(message.meta.as_ref().unwrap()["sentContent"], relative);
    assert_eq!(started.project_settings, Some(settings));
    let rule = started.config.unwrap().permissions.edit_approval;
    assert_eq!(rule.by, Approver::Ai, "the AI reviewer first");
}

/// R04: a chat of a project runs in its worktree on the host, which the
/// daemon names, so its start names no local folder, a drive letter least
/// of all; the editor context names files from the project's root, with
/// `/` between folders as the server's paths have. A daemon without
/// projects still gets the folder, which it runs in (--same-machine).
#[tokio::test]
async fn a_project_chats_start_names_no_local_folder() {
    let app = App::new();
    let root = r"C:\Users\me\project";
    app.core
        .db
        .write(move |c| rovik_core::chat_db::projects::ensure(c, "p1", "project", Path::new(root)))
        .await
        .unwrap();
    app.chat("chat", Some("p1"), Vec::new()).await;
    let fake = Fake::default();
    *fake.features.lock().unwrap() = vec![features::PROJECTS.into()];
    let (remote, fake) = connected_to(&app, fake).await;
    remote.place("chat", HOST);
    let context = format!(
        "[Active editor context — file {root}\\src\\ui\\x.rs (rust)]\n\
         See `{root}\\README.md`, C:/Users/me/project/docs\\a.md and {root}s\\b.md"
    );
    let mut request = start("chat", StartKind::Retry, Some(&context));
    request.root = Some(root.into());
    remote.start(request).await.unwrap();

    let started = starts(&fake).remove(0);
    assert_eq!(started.root, None);
    assert_eq!(
        started.current_user_content.as_deref(),
        Some(
            "[Active editor context — file src/ui/x.rs (rust)]\n\
             See `README.md`, docs/a.md and C:\\Users\\me\\projects\\b.md"
        )
    );

    let app = App::new();
    app.chat("chat", None, Vec::new()).await;
    let (remote, fake) = connected(&app).await;
    remote.place("chat", HOST);
    remote
        .start(start("chat", StartKind::Retry, None))
        .await
        .unwrap();
    assert_eq!(starts(&fake)[0].root.as_deref(), Some("/home/me/project"));
}

/// H04: a host that pushes its results is told the project's remote with
/// the project, as an SSH address without the credentials its `origin`
/// has; once it keeps them for the computer, the next sync tells it no
/// remote.
#[tokio::test]
async fn a_host_that_pushes_is_told_the_projects_remote() {
    use rovik_protocol::autopilot::Delivery;
    let app = App::new();
    let root = app.dir.0.join("project");
    std::fs::create_dir_all(&root).unwrap();
    let repo = git2::Repository::init(&root).unwrap();
    repo.remote("origin", "https://me:secret@github.com/acme/app.git")
        .unwrap();
    std::fs::write(root.join("a.txt"), "a\n").unwrap();
    let at = root.clone();
    app.core
        .db
        .write(move |c| rovik_core::chat_db::projects::ensure(c, "p1", "project", &at))
        .await
        .unwrap();
    app.chat("chat", Some("p1"), Vec::new()).await;
    let fake = Fake::default();
    *fake.features.lock().unwrap() = vec![features::PROJECTS.into(), features::AUTONOMY.into()];
    let (remote, fake) = connected_to(&app, fake).await;
    remote.place("chat", HOST);
    remote.set_policy(HOST, ApprovalPolicy::Autonomous);
    remote.set_autonomy(
        HOST,
        AutonomySettings {
            delivery: Delivery::PushBranch {
                deploy_key: "~/.ssh/rovik_deploy".into(),
                open_pr: false,
            },
            ..Default::default()
        },
    );
    let ensured = |fake: &Fake| -> Vec<Option<String>> {
        let requests = fake.requests.lock().unwrap();
        requests
            .iter()
            .filter_map(|r| match r {
                Request::ProjectEnsure { remote, .. } => Some(remote.clone()),
                _ => None,
            })
            .collect()
    };
    let send = |id: &str| {
        let mut request = start(
            "chat",
            StartKind::Send {
                message: UserMessage {
                    id: id.into(),
                    content: "Go".into(),
                    meta: None,
                    attachments: Vec::new(),
                },
            },
            None,
        );
        request.root = Some(root.to_string_lossy().into_owned());
        request
    };
    remote.start(send("u1")).await.unwrap();
    assert_eq!(
        ensured(&fake),
        [Some("git@github.com:acme/app.git".to_string())]
    );
    let shown = format!("{:?}", fake.requests.lock().unwrap());
    assert!(!shown.contains("me:secret"), "no credentials go: {shown}");

    remote.set_policy(HOST, ApprovalPolicy::AiReviewerThenMe);
    std::fs::write(root.join("a.txt"), "b\n").unwrap();
    remote.start(send("u2")).await.unwrap();
    assert_eq!(
        ensured(&fake),
        [Some("git@github.com:acme/app.git".to_string()), None],
        "the project again, with no remote"
    );
}

/// H05: "Test delivery" asks the host to push to the open project's
/// remote, as the host would push a result there: an SSH address without
/// the credentials its `origin` has. With no such project open, or a
/// daemon too old for it, it says what's missing.
#[tokio::test]
async fn a_delivery_test_goes_to_the_open_projects_remote() {
    let app = App::new();
    let root = app.dir.0.join("project");
    std::fs::create_dir_all(&root).unwrap();
    let repo = git2::Repository::init(&root).unwrap();
    repo.remote("origin", "https://me:secret@github.com/acme/app.git")
        .unwrap();
    let fake = Fake::default();
    *fake.features.lock().unwrap() = vec![features::AUTONOMY.into()];
    let (remote, fake) = connected_to(&app, fake).await;
    let none = remote.test_delivery(HOST).await.unwrap_err();
    assert!(
        none.starts_with("Open a project whose repository has an `origin`"),
        "{none}"
    );
    remote.set_open_project(Some(("p1".into(), root.clone())));
    let report = remote.test_delivery(HOST).await.unwrap();
    assert_eq!(report.remote, "git@github.com:acme/app.git");
    assert!(fake
        .requests
        .lock()
        .unwrap()
        .contains(&Request::DeliveryTest {
            remote: "git@github.com:acme/app.git".into()
        }));

    let (old, _) = connected(&app).await;
    old.set_open_project(Some(("p1".into(), root)));
    let too_old = old.test_delivery(HOST).await.unwrap_err();
    assert!(too_old.contains("too old"), "{too_old}");
}

/// K50: a Continue doesn't send the request's text again when it's the
/// stored one, local paths or not; other text still goes.
#[tokio::test]
async fn a_continue_doesnt_send_a_messages_text_again() {
    let app = App::new();
    let files = "<file name=\"notes.txt\">ten pages</file>\n\nSum them up";
    let mut asked = message("u1", "user", "Sum them up");
    asked.meta = Some(json!({ "sentContent": files }));
    app.chat("chat", None, vec![asked]).await;
    let (remote, fake) = connected(&app).await;
    remote.place("chat", HOST);
    let continued = start("chat", StartKind::ContinueReply, Some(files));
    remote.start(continued).await.unwrap();
    let retried = start("chat", StartKind::Retry, Some("Something else"));
    remote.start(retried).await.unwrap();
    let starts = starts(&fake);
    assert_eq!(starts[0].current_user_content, None);
    assert_eq!(
        starts[1].current_user_content.as_deref(),
        Some("Something else")
    );
}

/// K38: a project with a chat on a cloud host, or moving, isn't merged or
/// deleted: only the user moves a chat off a server.
#[tokio::test]
async fn a_project_with_a_remote_chat_isnt_merged_or_deleted() {
    let app = App::new();
    let (with_remote, all_local) = (project_in(&app, "a").await, project_in(&app, "b").await);
    app.chat("remote-chat", Some(&with_remote), Vec::new())
        .await;
    app.chat("local-chat", Some(&all_local), Vec::new()).await;
    let remote = Remote::new(app.core.clone());
    remote.place("remote-chat", HOST);
    settled(&app).await;
    let refused = remote.before_merging_project(&with_remote).await;
    assert_eq!(
        refused,
        Err(
            "This project has chats on test-server. Move them back to this computer before merging it."
                .into()
        )
    );
    let refused = remote.before_deleting_project(&with_remote).await;
    assert!(refused.unwrap_err().ends_with("before deleting it."));
    // The home host's chats are this computer's: but not this one's, on
    // a cloud host.
    remote.set_home(Some("wsl"));
    assert!(remote.before_deleting_project(&with_remote).await.is_err());
    assert_eq!(remote.before_deleting_project(&all_local).await, Ok(()));
    assert_eq!(remote.before_merging_project(&all_local).await, Ok(()));
    // Moving there: wait.
    let lock = |chat: &'static str| {
        app.core
            .db
            .write(move |c| moves::lock(c, chat, HOST, "move-1"))
    };
    lock("local-chat").await.unwrap();
    let refused = remote
        .before_deleting_project(&all_local)
        .await
        .unwrap_err();
    assert_eq!(
        refused,
        "A chat of this project is moving to test-server: wait for the move to end before deleting it."
    );
}

/// K38 for the home host (WSL on Windows), whose chats are this
/// computer's: deleting their project deletes them there too, as deleting
/// each would; merging it brings them back first, and is refused, with
/// why, while one can't come back.
#[tokio::test]
async fn a_projects_chats_on_the_home_host_go_with_it() {
    let app = App::new();
    let project = project_in(&app, "a").await;
    app.chat("in-wsl", Some(&project), Vec::new()).await;
    app.chat("here", Some(&project), Vec::new()).await;
    let fake = Fake::default();
    *fake.features.lock().unwrap() = vec![features::MIRROR.into(), features::PROJECTS.into()];
    let (remote, fake) = connected_to(&app, fake).await;
    remote.set_home(Some(HOST));
    remote.place("in-wsl", HOST);
    settled(&app).await;

    // Its run going there: merging waits for it.
    remote
        .lock()
        .running
        .insert("in-wsl".into(), "run-1".into());
    let refused = remote.before_merging_project(&project).await.unwrap_err();
    assert_eq!(
        refused,
        "A chat of this project is running in test-server: stop it, or let it finish, before merging it."
    );
    remote.lock().running.remove("in-wsl");

    // Its project's folder gone: it can't come back, so no merge.
    let id = project.clone();
    let folder = app
        .core
        .db
        .read(move |c| rovik_core::chat_db::projects::summary(c, &id))
        .await
        .unwrap()
        .unwrap()
        .path;
    std::fs::remove_dir_all(&folder).unwrap();
    let refused = remote.before_merging_project(&project).await.unwrap_err();
    assert!(
        refused.starts_with(
            "A chat of this project runs in test-server and has to come back to this computer \
             before merging it, but it can't: "
        ),
        "{refused}"
    );
    assert_eq!(remote.host_of("in-wsl").as_deref(), Some(HOST));

    // Deleted: there too, and here.
    remote.before_deleting_project(&project).await.unwrap();
    let requests = fake.requests.lock().unwrap().clone();
    assert!(
        requests.contains(&Request::ChatDelete {
            dialog_id: "in-wsl".into()
        }),
        "{requests:?}"
    );
    assert_eq!(remote.host_of("in-wsl"), None);
    let gone = app.core.db.read(|c| store::get_dialog(c, "in-wsl")).await;
    assert!(gone.unwrap().is_none());
    // The local chat is the project's delete's to take.
    let kept = app.core.db.read(|c| store::get_dialog(c, "here")).await;
    assert!(kept.unwrap().is_some());
}

/// A project of the folder `name` under the test's folder.
async fn project_in(app: &App, name: &str) -> String {
    let path = app.dir.0.join(name);
    std::fs::create_dir_all(&path).unwrap();
    let root = path.to_string_lossy().into_owned();
    let opened = rovik_core::chat_db::api::chat_open_project(&app.core.db, root).await;
    opened.unwrap().project.id
}

/// Every write queued so far is done: a chat's location, say, which
/// `place` queues.
async fn settled(app: &App) {
    let written = app.core.db.write(|_| Ok::<(), String>(()));
    written.await.unwrap();
}

/// 6.5: results land by themselves in the open project, which the user's
/// answer about its folder (a copy, or a project that moved) can change.
#[tokio::test]
async fn the_open_project_follows_what_the_user_says_its_folder_is() {
    let app = App::new();
    let remote = Remote::new(app.core.clone());
    let folder = app.dir.0.join("copy");
    remote.set_open_project(Some(("original".into(), folder.clone())));

    remote.folder_answered(&app.dir.0.join("elsewhere"), "separate");
    assert_eq!(
        remote.lock().open,
        Some(("original".into(), folder.clone()))
    );

    remote.folder_answered(&folder, "separate");
    assert_eq!(remote.lock().open, Some(("separate".into(), folder)));
}

/// D11, D16: a setting goes to every connected host, a configuration
/// without its keys.
#[tokio::test]
async fn a_configuration_goes_to_every_host_without_its_keys() {
    let app = App::new();
    let (remote, fake) = connected(&app).await;
    let config = AgentConfig {
        approval_profiles: vec![start("c", StartKind::Retry, None).profile],
        ..AgentConfig::default()
    };
    let reply = remote
        .everywhere(Request::AgentConfigure(Box::new(config.clone())))
        .await;
    assert_eq!(reply, Ok(Response::Done));
    let requests = fake.requests.lock().unwrap().clone();
    assert_eq!(
        requests,
        [Request::AgentConfigure(Box::new(config.without_keys()))]
    );
}

/// Cloud hosts' rows in Settings → Network: the hosts' configuration
/// carries how they connect, the proxy's password with it, and goes again
/// when it changes. In a process of its own, as it sets the process's
/// network settings.
#[test]
fn the_hosts_are_told_how_to_connect() {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "remote::tests::host_network_in_a_process_of_its_own",
        ])
        .args(["--ignored", "--test-threads=1"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("1 passed"), "{stdout}");
}

#[tokio::test]
#[ignore = "run in a process of its own by the_hosts_are_told_how_to_connect"]
async fn host_network_in_a_process_of_its_own() {
    use rovik_core::net::{
        HostNetwork, NetworkSettings, ProxyEndpoint, ProxyKind, ProxyRoute, Route, Routes,
        SavedProxy,
    };
    use rovik_protocol::Secret;
    use std::collections::HashMap;

    let configures = |fake: &Fake| -> Vec<AgentConfig> {
        let requests = fake.requests.lock().unwrap().clone();
        requests
            .into_iter()
            .filter_map(|request| match request {
                Request::AgentConfigure(config) => Some(*config),
                _ => None,
            })
            .collect()
    };
    let app = App::new();
    let (remote, fake) = connected(&app).await;
    let mut settings = NetworkSettings {
        timeout_secs: Some(45),
        proxies: vec![SavedProxy {
            id: "corp".into(),
            kind: ProxyKind::Http,
            host: "proxy.example.com".into(),
            port: 3128,
            username: Some("alice".into()),
            password_saved: false,
        }],
        routes: Routes {
            host_models: Route::Proxy("corp".into()),
            host_browser: Route::Proxy("corp".into()),
            ..Routes::default()
        },
    };
    let password = HashMap::from([("corp".to_string(), Some(Secret("pw".into())))]);
    rovik_core::net::save(settings.clone(), &password).unwrap();
    let corp = ProxyRoute::Proxy(ProxyEndpoint {
        kind: ProxyKind::Http,
        host: "proxy.example.com".into(),
        port: 3128,
        username: Some("alice".into()),
        password: Some(Secret("pw".into())),
    });

    remote
        .everywhere(Request::AgentConfigure(Box::default()))
        .await
        .unwrap();
    assert_eq!(
        configures(&fake).last().unwrap().network,
        Some(HostNetwork {
            timeout_secs: Some(45),
            models: Some(corp.clone()),
            browser: Some(corp.clone()),
        })
    );
    // Not this computer's: the configuration here has none, and its
    // browser goes its own way.
    assert_eq!(app.core.agents.config().network, None);
    assert_eq!(rovik_core::net::browser_proxy().await, None);

    // The hosts' browsers go directly now: they're told.
    settings.routes.host_browser = Route::Direct;
    rovik_core::net::save(settings, &HashMap::new()).unwrap();
    remote.send_host_network().await;
    let sent = configures(&fake);
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[1].network,
        Some(HostNetwork {
            timeout_secs: Some(45),
            models: Some(corp),
            browser: Some(ProxyRoute::Direct),
        })
    );
}

/// The fake's requests once the connection caught up (its `AgentStatus`),
/// those before it kept.
async fn requests_once_caught_up(fake: &Fake) -> Vec<Request> {
    for _ in 0..400 {
        let requests = fake.requests.lock().unwrap().clone();
        if requests.contains(&Request::AgentStatus) {
            return requests;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the fake daemon never answered");
}

/// §2.3: a host connecting after the settings were applied (one just set
/// up, or back from being away) is sent them on connecting, before
/// anything else, without keys.
#[tokio::test]
async fn a_host_connecting_after_the_settings_were_applied_gets_them() {
    let app = App::new();
    let remote = Remote::new(app.core.clone());
    let config = AgentConfig {
        approval_profiles: vec![start("c", StartKind::Retry, None).profile],
        ..AgentConfig::default()
    };
    // Applied with no host connected: nowhere to send it yet.
    let reply = remote
        .everywhere(Request::AgentConfigure(Box::new(config.clone())))
        .await;
    assert_eq!(reply, Ok(Response::Done));
    let fake = Fake::default();
    remote.connect(HOST, fake.clone(), identity(HOST));
    let requests = requests_once_caught_up(&fake).await;
    assert_eq!(
        requests[..2],
        [
            Request::AgentConfigure(Box::new(config.without_keys())),
            Request::AgentStatus
        ]
    );
}

/// A host connecting before any settings were applied isn't sent a
/// configuration nobody set: it keeps its own until the frontend's comes.
#[tokio::test]
async fn a_host_connecting_first_keeps_its_configuration() {
    let app = App::new();
    let remote = Remote::new(app.core.clone());
    let fake = Fake::default();
    remote.connect(HOST, fake.clone(), identity(HOST));
    let requests = requests_once_caught_up(&fake).await;
    assert!(
        !requests
            .iter()
            .any(|r| matches!(r, Request::AgentConfigure(_))),
        "{requests:?}"
    );
}

#[test]
fn a_hosts_policy_sends_what_waits_for_the_user_to_the_ai_reviewer() {
    let mut config = AgentConfig {
        permissions: Permissions {
            command_approval: ApprovalRule {
                required: true,
                by: Approver::User,
            },
            ..Permissions::default()
        },
        ..AgentConfig::default()
    };
    // Without an AI reviewer, it waits for the user whatever the policy.
    let mut alone = config.clone();
    ApprovalPolicy::AiReviewerThenMe.apply(&AutonomySettings::default(), &mut alone);
    assert_eq!(alone, config);
    config.approval_profiles = vec![start("c", StartKind::Retry, None).profile];
    let mut waiting = config.clone();
    ApprovalPolicy::WaitForMe.apply(&AutonomySettings::default(), &mut waiting);
    assert_eq!(waiting, config);
    ApprovalPolicy::AiReviewerThenMe.apply(&AutonomySettings::default(), &mut config);
    assert_eq!(config.permissions.command_approval.by, Approver::Ai);
    assert!(
        !config.permissions.edit_approval.required,
        "what needs no approval still doesn't"
    );
}

/// Q11: a host's policy covers the MCP rule as it covers the others: what
/// asks the user for an MCP tool goes to the AI reviewer where a command
/// would, and waits for the user where a command would.
#[test]
fn a_hosts_policy_treats_the_mcp_rule_as_the_others() {
    let asks_the_user = ApprovalRule {
        required: true,
        by: Approver::User,
    };
    let config = AgentConfig {
        permissions: Permissions {
            command_approval: asks_the_user,
            mcp_approval: asks_the_user,
            ..Permissions::default()
        },
        approval_profiles: vec![start("c", StartKind::Retry, None).profile],
        ..AgentConfig::default()
    };
    for (policy, by) in [
        (ApprovalPolicy::AiReviewerThenMe, Approver::Ai),
        (ApprovalPolicy::Autonomous, Approver::Ai),
        (ApprovalPolicy::WaitForMe, Approver::User),
    ] {
        let mut applied = config.clone();
        policy.apply(&AutonomySettings::default(), &mut applied);
        let p = &applied.permissions;
        assert_eq!(p.command_approval.by, by, "{policy:?}");
        assert_eq!(p.mcp_approval.by, by, "{policy:?}: as a command");
    }
}

/// H02: the configuration a host that works on its own gets: what would
/// wait for the user goes to the AI reviewer, and the block's effort and
/// budget, each where it sets one, are over the computer's. A message that
/// names no mode runs in Autopilot there; one that names another keeps
/// it. The host keeps the block's delivery and notify URL, sent on
/// connecting and at each change, and forgets them once it no longer works
/// on its own.
#[tokio::test]
async fn a_host_that_works_on_its_own_gets_its_block() {
    use rovik_protocol::autopilot::{AutonomyConfig, Budget, Delivery, Effort};
    let app = App::new();
    app.chat("chat", None, Vec::new()).await;
    let mut config = AgentConfig::default();
    config.permissions.command_approval = ApprovalRule {
        required: true,
        by: Approver::User,
    };
    config.approval_profiles = vec![start("c", StartKind::Retry, None).profile];
    config.autopilot.budget = Budget {
        max_hours: Some(2.0),
        max_cost: Some(5.0),
        max_tokens: Some(3_000_000),
    };
    let block = AutonomySettings {
        effort: Some(Effort::Thorough),
        budget: Some(Budget {
            max_hours: Some(8.0),
            max_cost: None,
            max_tokens: None,
        }),
        delivery: Delivery::PushBranch {
            deploy_key: "~/.ssh/rovik_deploy".into(),
            open_pr: true,
        },
        notify_url: Some("https://ntfy.sh/rovik-topic".into()),
        plan_fix: None,
    };
    let remote = Remote::new(app.core.clone());
    remote.set_policy(HOST, ApprovalPolicy::Autonomous);
    remote.set_autonomy(HOST, block.clone());
    remote
        .everywhere(Request::AgentConfigure(Box::new(config.clone())))
        .await
        .unwrap();
    let fake = Fake::default();
    *fake.features.lock().unwrap() = vec![features::AUTONOMY.into()];
    remote.connect(HOST, fake.clone(), identity(HOST));
    let requests = requests_once_caught_up(&fake).await;

    // On connecting: the configuration, then what the host keeps.
    let Request::AgentConfigure(received) = &requests[0] else {
        panic!("{requests:?}");
    };
    assert_eq!(received.permissions.command_approval.by, Approver::Ai);
    assert_eq!(received.autopilot.effort, Effort::Thorough);
    assert_eq!(
        received.autopilot.budget,
        Budget {
            max_hours: Some(8.0),
            max_cost: Some(5.0),
            max_tokens: Some(3_000_000),
        },
        "the host's hours, the computer's cost and tokens"
    );
    let mut rest = received.as_ref().clone();
    rest.permissions.command_approval.by = Approver::User;
    rest.autopilot = config.autopilot.clone();
    assert_eq!(rest, config.without_keys(), "the rest is the computer's");
    assert_eq!(requests[1], Request::AutonomyConfigure(block.config()));

    // A start: under the same configuration, in Autopilot unless it says.
    remote.place("chat", HOST);
    let mut request = start("chat", StartKind::Retry, None);
    request.mode = None;
    request.config = Some(config.clone());
    remote.start(request.clone()).await.unwrap();
    request.mode = Some("plan".into());
    remote.start(request).await.unwrap();
    let started = starts(&fake);
    assert_eq!(started[0].mode.as_deref(), Some("autopilot"));
    assert_eq!(started[1].mode.as_deref(), Some("plan"), "what it named");
    // With its keys, which a start carries (D11).
    let start_config = started[0].config.as_ref().map(AgentConfig::without_keys);
    assert_eq!(start_config.as_ref(), Some(received.as_ref()));

    // A change reaches it at once; "Wait for me" forgets the block there.
    fake.requests.lock().unwrap().clear();
    remote.set_policy(HOST, ApprovalPolicy::WaitForMe);
    remote.reconfigure(HOST).await;
    // What reconfiguring sent, among the pulls of the chat its starts may
    // still have going.
    let requests: Vec<Request> = (fake.requests.lock().unwrap().iter())
        .filter(|r| {
            matches!(
                r,
                Request::AgentConfigure(_) | Request::AutonomyConfigure(_)
            )
        })
        .cloned()
        .collect();
    let Request::AgentConfigure(waiting) = &requests[0] else {
        panic!("{requests:?}");
    };
    assert_eq!(
        waiting.as_ref(),
        &config.without_keys(),
        "the computer's, as it is"
    );
    assert_eq!(
        requests[1],
        Request::AutonomyConfigure(AutonomyConfig::default())
    );
}

/// A host of the settings has a UUID for its id: what the user reads names
/// it as its settings do.
#[tokio::test]
async fn messages_name_a_host_as_its_settings_do() {
    let app = App::new();
    app.chat("chat", None, Vec::new()).await;
    let remote = Remote::new(app.core.clone());
    let id = "3f2a9c41-host";
    remote.connect(id, Fake::default(), identity("Build server"));
    for _ in 0..400 {
        if remote
            .hosts
            .client(id)
            .is_some_and(|c| c.state() == HostState::Ready)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let notice = AgentEvent::Notice {
        dialog_id: None,
        kind: "warning".into(),
        title: "Stopped".into(),
        message: "Why".into(),
    };
    remote.receive(id, event(notice)).await;
    assert_eq!(app.sent()[0]["title"], "Build server: Stopped");
    let preflight = remote.move_preflight("chat", id).await.unwrap();
    assert_eq!(
        preflight.blocked.as_deref(),
        Some(
            "Build server runs an older Rovik, which can't take chats: it needs an upgrade first."
        )
    );
}

/// R03, T05: a chat whose task's candidates implement it, or wait for
/// their Pick, doesn't move: their working copies can't go with it.
#[tokio::test]
async fn a_chat_whose_candidates_await_their_pick_doesnt_move() {
    use rovik_core::agent_run::autopilot::{add_stage, end_stage, Stage, StageRecord, Status};
    use rovik_core::agent_run::candidates::MOVE_AWAITS_PICK;
    use rovik_protocol::autopilot::Effort;
    let app = App::new();
    app.chat("chat", None, Vec::new()).await;
    // A host that takes chats.
    let fake = Fake::default();
    *fake.features.lock().unwrap() = vec![features::MIRROR.into()];
    let (remote, _fake) = connected_to(&app, fake).await;
    let core = &app.core;
    let done = |name| StageRecord::ended("task", name, 1, Effort::Thorough, Status::Done);
    for name in [Stage::Plan, Stage::PlanReview, Stage::Prepare, Stage::Tests] {
        add_stage(core, "chat", &done(name)).await.unwrap();
    }
    let implementing = StageRecord {
        candidates: vec!["a".into(), "b".into()],
        ..StageRecord::running("task", Stage::Implement, 1, Effort::Thorough)
    };
    let implement = add_stage(core, "chat", &implementing).await.unwrap();
    let blocked = || async { remote.move_preflight("chat", HOST).await.unwrap().blocked };
    assert_eq!(blocked().await.as_deref(), Some(MOVE_AWAITS_PICK));
    // Implemented, and their Pick under way: still.
    end_stage(core, "chat", &implement, |r| r.status = Status::Done)
        .await
        .unwrap();
    let picking = StageRecord::running("task", Stage::Pick, 1, Effort::Thorough);
    let pick = add_stage(core, "chat", &picking).await.unwrap();
    assert_eq!(blocked().await.as_deref(), Some(MOVE_AWAITS_PICK));
    let moved = remote.move_chat("chat", HOST, None).await.unwrap_err();
    assert_eq!(moved.message, MOVE_AWAITS_PICK);
    // Picked: the copies are done with, and it moves as any chat would.
    end_stage(core, "chat", &pick, |r| r.status = Status::Done)
        .await
        .unwrap();
    assert_ne!(blocked().await.as_deref(), Some(MOVE_AWAITS_PICK));

    // Nor back from a server, its mirror's task's candidates at work there.
    app.chat("there", None, Vec::new()).await;
    remote.place("there", HOST);
    for name in [Stage::Plan, Stage::PlanReview, Stage::Prepare, Stage::Tests] {
        add_stage(core, "there", &done(name)).await.unwrap();
    }
    add_stage(core, "there", &implementing).await.unwrap();
    let back = remote.move_back("there", None).await.unwrap_err();
    assert_eq!(back.message, MOVE_AWAITS_PICK);
}

/// R02, 4.5: a host whose Rovik is too old to take chats blocks the move.
/// An administrator there is offered the upgrade for everyone; anyone
/// else is told whom to ask.
#[tokio::test]
async fn a_host_too_old_for_chats_says_who_upgrades_it() {
    let app = App::new();
    app.chat("chat", None, Vec::new()).await;
    let (remote, fake) = connected(&app).await;
    let info = |user: &str| rovik_protocol::host::HostInfo {
        version: "0.1.0".into(),
        user: user.into(),
        install: Some(rovik_protocol::host::Install {
            admins: vec!["ann".into(), "bo".into()],
        }),
        ..Default::default()
    };
    *fake.info.lock().unwrap() = Some(info("cy"));
    let preflight = remote.move_preflight("chat", HOST).await.unwrap();
    assert_eq!(
        preflight.blocked.as_deref(),
        Some("test-server runs Rovik 0.1.0, which can't take chats: ask ann or bo to upgrade it.")
    );
    assert!(!preflight.upgrade);

    *fake.info.lock().unwrap() = Some(info("bo"));
    let preflight = remote.move_preflight("chat", HOST).await.unwrap();
    assert_eq!(
        preflight.blocked.as_deref(),
        Some(
            "test-server runs Rovik 0.1.0, which can't take chats: upgrade it for everyone first."
        )
    );
    assert!(preflight.upgrade);
    let moved = remote.move_chat("chat", HOST, None).await.unwrap_err();
    assert_eq!(moved.code, FailCode::Version);
}

/// K46, 4.5: a daemon that doesn't name the attachments feature would send
/// the message to the model without its files: it isn't sent at all.
#[tokio::test]
async fn files_arent_sent_to_a_daemon_that_doesnt_take_them() {
    let app = App::new();
    app.chat("chat", None, Vec::new()).await;
    let stored = attachments::store_attached(&app.core.db, b"%PDF-1.4\n%%EOF".to_vec())
        .await
        .unwrap();
    let (remote, fake) = connected(&app).await;
    remote.place("chat", HOST);
    let send = StartKind::Send {
        message: UserMessage {
            id: "u1".into(),
            content: "Read it".into(),
            meta: None,
            attachments: vec![stored.id],
        },
    };
    let refused = remote.start(start("chat", send, None)).await.unwrap_err();
    assert_eq!(refused.code, FailCode::Version);
    assert!(refused
        .message
        .starts_with("test-server runs a Rovik too old"));
    assert!(fake.requests.lock().unwrap().is_empty(), "nothing sent");
}

/// K44: a file the host can't send doesn't hold its row back: the row goes
/// in with a row for the file, which requests say is no longer shown.
#[tokio::test]
async fn a_file_the_host_cant_send_leaves_its_row_without_it() {
    let app = App::new();
    app.chat("chat", None, Vec::new()).await;
    let (remote, _fake) = connected(&app).await;
    remote.place("chat", HOST);
    let lost = "f".repeat(64);
    let mut shot = message("t1", "tool", "Screenshot");
    shot.tool_call_id = Some("c1".into());
    shot.attachments = vec![lost.clone()];
    let added = AgentEvent::MessagesAdded {
        dialog_id: "chat".into(),
        messages: vec![events::out_message("chat", 1, &shot, false)],
    };
    remote.receive(HOST, event(added)).await;
    let rows = app.rows("chat").await;
    assert_eq!(rows[0].attachments, std::slice::from_ref(&lost));
    let data = app.core.db.data_dir().to_path_buf();
    assert!(attachments::file_for(&data, &lost).is_none());
    assert_eq!(*app.witnessed.lock().unwrap(), [("t1".to_string(), true)]);
}

/// A host that comes back tells of its tasks' drivers at work before its
/// runs, and of those that finished while it was away after them: between
/// two of a task's runs, its stage never shows as cut short.
#[tokio::test]
async fn a_host_that_comes_back_tells_of_its_drivers_at_work() {
    let app = App::new();
    for chat in ["going", "done"] {
        app.chat(chat, None, Vec::new()).await;
    }
    let (remote, fake) = connected(&app).await;
    for chat in ["going", "done"] {
        remote.place(chat, HOST);
    }
    let driving = |dialog_id: &str, driving| {
        event(AgentEvent::TaskDriving {
            dialog_id: dialog_id.into(),
            driving,
        })
    };
    remote.receive(HOST, driving("done", true)).await;
    *fake.status.lock().unwrap() = Some(AgentStatus {
        runs: vec![RunningRun {
            dialog_id: "going".into(),
            run_id: "r1".into(),
            steerable: false,
        }],
        driving: vec!["going".into()],
        ..Default::default()
    });
    app.events.lock().unwrap().clear();
    remote
        .receive(HOST, Incoming::Subscribed { complete: false })
        .await;
    let told: Vec<String> = app
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| {
            matches!(
                e["type"].as_str(),
                Some("taskDriving" | "runStarted" | "runEnded")
            )
        })
        .map(|e| format!("{} {} {}", e["type"], e["dialogId"], e["driving"]))
        .collect();
    assert_eq!(
        told,
        [
            r#""taskDriving" "going" true"#,
            r#""runStarted" "going" null"#,
            r#""taskDriving" "done" false"#,
        ]
    );
    // Told once: the next catch-up finds nothing new to tell of "done".
    app.events.lock().unwrap().clear();
    *fake.status.lock().unwrap() = Some(AgentStatus::default());
    remote
        .receive(HOST, Incoming::Subscribed { complete: false })
        .await;
    let ended: Vec<Value> = app
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "taskDriving")
        .cloned()
        .collect();
    assert_eq!(
        ended,
        [json!({ "type": "taskDriving", "dialogId": "going", "driving": false })]
    );
}

#[tokio::test]
async fn a_host_that_goes_away_has_to_catch_up_again() {
    let app = App::new();
    let (remote, _fake) = connected(&app).await;
    for _ in 0..400 {
        if remote.caught_up(HOST) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(remote.caught_up(HOST), "its chats pulled: it has none");
    remote
        .receive(HOST, Incoming::State(HostState::Offline))
        .await;
    assert!(!remote.caught_up(HOST), "away, it has to catch up again");
}

/// One window per project (the app's instances.rs): a remote chat whose
/// project another window has open shows its events there, not in this
/// one; once this window opens it, here; one open nowhere, in every one.
#[tokio::test]
async fn a_chat_shows_its_events_in_the_window_its_project_is_open_in() {
    let app = App::new();
    let folder = app.dir.0.join("p");
    std::fs::create_dir_all(&folder).unwrap();
    let root = folder.to_string_lossy().into_owned();
    let opened = rovik_core::chat_db::api::chat_open_project(&app.core.db, root).await;
    let project = opened.unwrap().project.id;
    app.chat("chat", Some(&project), Vec::new()).await;
    let remote = Remote::new(app.core.clone());
    remote.place("chat", HOST);
    let open_there = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let there = open_there.clone();
    let id = project.clone();
    remote.on_open_elsewhere(Arc::new(move |p| {
        p == id && there.load(std::sync::atomic::Ordering::Relaxed)
    }));
    let ask = || AgentEvent::ApprovalRequested {
        request: approval("chat", false),
    };
    remote.receive(HOST, event(ask())).await;
    assert!(
        app.sent().is_empty(),
        "the other window's: {:?}",
        app.sent()
    );
    assert!(remote.handled_elsewhere("chat").await);
    assert_eq!(
        remote.check_here("chat").await,
        Err(
            "This chat's project is open in another Rovik window: land or discard it there.".into()
        )
    );

    // Open here now.
    remote.set_open_project(Some((project.clone(), folder)));
    remote.receive(HOST, event(ask())).await;
    assert_eq!(app.sent().len(), 1);
    assert!(remote.check_here("chat").await.is_ok());

    // Open nowhere: every window's.
    remote.set_open_project(None);
    open_there.store(false, std::sync::atomic::Ordering::Relaxed);
    remote.lock().elsewhere_seen.clear();
    remote.receive(HOST, event(ask())).await;
    assert_eq!(app.sent().len(), 2);
}

/// Another window may have changed a host's policy: before a configuration
/// goes to a host, the policies are brought up to date, so this window's
/// old one isn't sent back.
#[tokio::test]
async fn the_policies_are_read_again_before_a_configuration_goes_out() {
    let app = App::new();
    let (remote, fake) = connected(&app).await;
    remote.set_policy(HOST, ApprovalPolicy::AiReviewerThenMe);
    let mine = Arc::downgrade(&remote);
    remote.on_refresh(Arc::new(move || {
        // As HostsPage::follow_policies finds the file another window wrote.
        if let Some(remote) = mine.upgrade() {
            remote.set_policy(HOST, ApprovalPolicy::WaitForMe);
        }
    }));
    let config = AgentConfig {
        approval_profiles: vec![start("c", StartKind::Retry, None).profile],
        ..AgentConfig::default()
    };
    remote
        .everywhere(Request::AgentConfigure(Box::new(config.clone())))
        .await
        .unwrap();
    assert_eq!(remote.policy(HOST), ApprovalPolicy::WaitForMe);
    let mut expected = config.without_keys();
    ApprovalPolicy::WaitForMe.apply(&AutonomySettings::default(), &mut expected);
    let requests = fake.requests.lock().unwrap().clone();
    assert_eq!(requests, [Request::AgentConfigure(Box::new(expected))]);
}

/// A mirrored chat is pulled by one window at a time: two pulling from the
/// same revision could write an older page over a newer one.
#[tokio::test]
async fn a_chat_is_pulled_by_one_window_at_a_time() {
    let app = App::new();
    let (one, other) = (Remote::new(app.core.clone()), Remote::new(app.core.clone()));
    let held = one.pull_lock("chat").await.unwrap();
    assert!(held.is_some());
    let waiting = tokio::spawn(async move { other.pull_lock("chat").await.map(|l| l.is_some()) });
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        !waiting.is_finished(),
        "the other window waits for the pull going"
    );
    drop(held);
    let took = tokio::time::timeout(Duration::from_secs(5), waiting).await;
    assert_eq!(took.unwrap().unwrap(), Ok(true));
}

/// steering-plan.md 3.8: a message for a run on a host goes there with the
/// editor context's local paths written from the open project's folder,
/// as a start's are (K08), and the host's status adds what waits there.
#[tokio::test]
async fn a_steer_goes_to_the_chats_host_with_its_paths_from_the_project() {
    let app = App::new();
    app.chat("chat", None, Vec::new()).await;
    let (remote, fake) = connected(&app).await;
    remote.place("chat", HOST);
    let folder = app.dir.0.join("project");
    remote.set_open_project(Some(("p".into(), folder.clone())));
    let local = folder.join("src").join("a.rs");
    let message = UserMessage {
        id: "s1".into(),
        content: "use the client".into(),
        meta: Some(serde_json::json!({
            "sentContent": format!("[Active editor context — file {}]\n\nuse the client", local.display())
        })),
        attachments: Vec::new(),
    };
    remote.steer("chat", message.clone()).await.unwrap();
    let sent = fake
        .requests
        .lock()
        .unwrap()
        .iter()
        .find_map(|r| match r {
            Request::AgentSteer { dialog_id, message } if dialog_id == "chat" => {
                Some(message.clone())
            }
            _ => None,
        })
        .expect("sent to the host");
    assert_eq!(
        sent.meta.unwrap()["sentContent"],
        "[Active editor context — file src/a.rs]\n\nuse the client"
    );
    assert_eq!(sent.content, "use the client");

    // What waits there shows in the status.
    *fake.status.lock().unwrap() = Some(AgentStatus {
        runs: vec![RunningRun {
            dialog_id: "chat".into(),
            run_id: "r1".into(),
            steerable: true,
        }],
        steering: vec![rovik_protocol::agent::QueuedSteer {
            dialog_id: "chat".into(),
            message,
            queued_at: 1,
        }],
        ..Default::default()
    });
    let status = remote.status().await.unwrap();
    assert_eq!(status.steering.len(), 1);
    assert!(status.runs.iter().any(|r| r.steerable));
    // Taken back there too.
    remote.unsteer("chat", "s1".into()).await.unwrap();
    assert!(fake.requests.lock().unwrap().iter().any(|r| matches!(
        r,
        Request::AgentUnsteer { message_id, .. } if message_id == "s1"
    )));
}

/// A host's plan-fix limits replace the computer's field by field.
#[test]
fn a_hosts_plan_fix_changes_only_the_fields_it_sets() {
    use rovik_protocol::autopilot::{PlanFix, PlanFixOverride};
    let mut config = AgentConfig::default();
    config.autopilot.plan_fix = PlanFix {
        max_rounds: 10,
        stall_rounds: 3,
        max_minutes: 45.0,
        budget_share: 0.5,
        max_retries: 1,
    };
    let autonomy: AutonomySettings =
        serde_json::from_value(serde_json::json!({ "planFix": { "maxRounds": 5 } })).unwrap();
    assert_eq!(
        autonomy.plan_fix,
        Some(PlanFixOverride {
            max_rounds: Some(5),
            ..Default::default()
        })
    );
    let mut waiting = config.clone();
    ApprovalPolicy::WaitForMe.apply(&autonomy, &mut waiting);
    assert_eq!(waiting.autopilot.plan_fix.max_rounds, 10, "only a host that works on its own");
    ApprovalPolicy::Autonomous.apply(&autonomy, &mut config);
    assert_eq!(
        config.autopilot.plan_fix,
        PlanFix {
            max_rounds: 5,
            stall_rounds: 3,
            max_minutes: 45.0,
            budget_share: 0.5,
            max_retries: 1,
        }
    );
}

/// An MCP server over HTTP with one secret header, `s1`'s `Authorization`.
fn mcp_server() -> rovik_protocol::mcp::McpServer {
    use rovik_protocol::mcp::{McpServer, McpTransport, McpValue};
    McpServer {
        id: "s1".into(),
        name: "github".into(),
        enabled: true,
        transport: McpTransport::Url {
            url: "http://127.0.0.1:1/mcp".into(),
            headers: vec![McpValue {
                name: "Authorization".into(),
                value: String::new(),
                secret: true,
            }],
        },
        disabled_tools: Vec::new(),
    }
}

/// The app with `s1`'s `Authorization` saved as `saved` (an error when
/// `Err`), and a connected fake host that announces `served`.
async fn mcp_host(
    saved: Result<&'static str, &'static str>,
    served: &[&str],
) -> (App, Arc<Remote>, Fake) {
    let app = App::new();
    app.chat("chat", None, Vec::new()).await;
    app.core.mcp.set_secrets(Arc::new(move |id, key| {
        assert_eq!((id, key), ("s1", "header/Authorization"));
        saved.map(|v| Some(v.to_string())).map_err(str::to_string)
    }));
    let fake = Fake::default();
    *fake.features.lock().unwrap() = served.iter().map(|f| f.to_string()).collect();
    let (remote, fake) = connected_to(&app, fake).await;
    remote.place("chat", HOST);
    (app, remote, fake)
}

/// Q11 (mcp-plan-2.md): a host gets the MCP servers in its configuration
/// and its starts, and a start carries their saved secrets.
#[tokio::test]
async fn a_host_is_sent_the_mcp_servers_and_a_start_their_secrets() {
    let (_app, remote, fake) = mcp_host(Ok("Bearer x"), &[features::MCP]).await;
    let config = AgentConfig {
        mcp_servers: vec![mcp_server()],
        ..AgentConfig::default()
    };
    remote
        .everywhere(Request::AgentConfigure(Box::new(config.clone())))
        .await
        .unwrap();
    let configured = fake.requests.lock().unwrap().clone();
    let [Request::AgentConfigure(sent)] = &configured[..] else {
        panic!("{configured:?}");
    };
    assert_eq!(sent.mcp_servers, config.mcp_servers);

    let mut request = start("chat", StartKind::Retry, None);
    request.config = Some(config.clone());
    remote.start(request).await.unwrap();
    let started = starts(&fake).remove(0);
    assert_eq!(started.config.unwrap().mcp_servers, config.mcp_servers);
    assert_eq!(
        started.mcp_secrets["s1"]["header/Authorization"].0,
        "Bearer x"
    );
}

#[tokio::test]
async fn an_older_host_is_sent_no_mcp_secrets() {
    let (_app, remote, fake) = mcp_host(Ok("Bearer x"), &[]).await;
    let mut request = start("chat", StartKind::Retry, None);
    request.config = Some(AgentConfig {
        mcp_servers: vec![mcp_server()],
        ..AgentConfig::default()
    });
    remote.start(request).await.unwrap();
    assert!(starts(&fake)[0].mcp_secrets.is_empty());
}

#[tokio::test]
async fn a_start_goes_when_a_secret_can_t_be_read() {
    let (_app, remote, fake) = mcp_host(Err("locked"), &[features::MCP]).await;
    let mut request = start("chat", StartKind::Retry, None);
    request.config = Some(AgentConfig {
        mcp_servers: vec![mcp_server()],
        ..AgentConfig::default()
    });
    remote.start(request).await.unwrap();
    assert!(!starts(&fake)[0].mcp_secrets.contains_key("s1"));
}

#[tokio::test]
async fn mcp_test_on_a_host_sends_typed_values_over_saved_ones() {
    let (_app, remote, fake) = mcp_host(Ok("old"), &[features::MCP]).await;
    let given = BTreeMap::from([("header/Authorization".to_string(), Secret("new".into()))]);
    remote.mcp_test(HOST, mcp_server(), given).await.unwrap();
    let requests = fake.requests.lock().unwrap().clone();
    let [Request::McpTest { values, .. }] = &requests[..] else {
        panic!("{requests:?}");
    };
    assert_eq!(values["header/Authorization"].0, "new");

    // With nothing typed, the saved value goes.
    fake.requests.lock().unwrap().clear();
    remote
        .mcp_test(HOST, mcp_server(), BTreeMap::new())
        .await
        .unwrap();
    let requests = fake.requests.lock().unwrap().clone();
    let [Request::McpTest { values, .. }] = &requests[..] else {
        panic!("{requests:?}");
    };
    assert_eq!(values["header/Authorization"].0, "old");
}

#[tokio::test]
async fn mcp_test_on_an_older_host_says_to_update_it() {
    let (_app, remote, fake) = mcp_host(Ok("old"), &[]).await;
    let fail = remote
        .mcp_test(HOST, mcp_server(), BTreeMap::new())
        .await
        .unwrap_err();
    assert_eq!(
        fail.message,
        "test-server runs an older Rovik: update it to test MCP servers there."
    );
    assert!(fake.requests.lock().unwrap().is_empty());
}
