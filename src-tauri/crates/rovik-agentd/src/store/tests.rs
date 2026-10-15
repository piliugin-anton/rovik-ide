//! The store and its keys (P05).

use super::*;
use rovik_core::testing::temp_dir;
use rovik_protocol::agent::{StartKind, UserMessage};
use rovik_protocol::permissions::{ApprovalRule, Approver};
use rovik_protocol::subagents::{KindConfig, ModelChoice};
use std::fs;
use std::os::unix::fs::PermissionsExt;

const OPENAI: &str = "https://api.openai.example/v1";
const OTHER: &str = "https://models.other.example/v1";
const THIRD: &str = "https://third.example/v1";

fn model(id: &str, base_url: &str, key: &str) -> ModelProfile {
    ModelProfile {
        id: id.into(),
        label: id.into(),
        model: id.into(),
        base_url: base_url.into(),
        api_key: key.into(),
        stream: true,
        vision: false,
        pdf: false,
        context_tokens: None,
        no_tools: false,
        first_byte_timeout_ms: None,
        api_route: None,
    }
}

/// A configuration whose reviewer and scout, each of a provider of its
/// own, carry keys (or not, with "").
fn config(reviewer_key: &str, scout_key: &str) -> AgentConfig {
    let mut config = AgentConfig::default();
    config.approval_profiles = vec![model("other::reviewer", OTHER, reviewer_key)];
    config.subagents.kinds.insert(
        "scout".into(),
        KindConfig {
            enabled: true,
            models: vec![
                ModelChoice::Profile(model("third::mini", THIRD, scout_key)),
                ModelChoice::Chat { chat: true },
            ],
            ..Default::default()
        },
    );
    config
}

fn start(dialog: &str, key: &str, config: Option<AgentConfig>) -> StartRequest {
    StartRequest {
        dialog_id: dialog.into(),
        start: StartKind::Send {
            message: UserMessage {
                id: "u".into(),
                content: "The message's own text".into(),
                meta: None,
                attachments: Vec::new(),
            },
        },
        profile: model("openai::gpt", OPENAI, key),
        options: None,
        mode: Some("agent".into()),
        context_mode: None,
        root: Some("/p".into()),
        current_user_content: Some("The message's own text, with context".into()),
        local_date: None,
        config,
        isolate: None,
        project_settings: None,
    }
}

/// Every file under `dir` but keys.json, as text.
fn others(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(others(&path));
        } else if path.file_name().is_some_and(|n| n != "keys.json") {
            out.push((
                path.clone(),
                String::from_utf8_lossy(&fs::read(&path).unwrap()).into(),
            ));
        }
    }
    out
}

#[test]
fn keys_are_kept_in_keys_json_alone() {
    let dir = temp_dir();
    let store = Store::open(&dir.0).unwrap();
    let secrets = ["sk-chat-secret", "sk-reviewer-secret", "sk-scout-secret"];
    let started = start("chat", secrets[0], Some(config(secrets[1], secrets[2])));
    store.started("laptop", &started).unwrap();
    let keys = fs::read_to_string(dir.0.join("keys.json")).unwrap();
    for secret in secrets {
        assert!(keys.contains(secret), "{secret} kept");
        for (path, text) in others(&dir.0) {
            assert!(!text.contains(secret), "{secret} in {path:?}");
        }
    }
    let state = fs::read_to_string(dir.0.join("state.json")).unwrap();
    assert!(!state.contains("The message's own text"), "{state}");
    // The runs' configuration has them, in memory.
    let config = store.config_of("chat").unwrap();
    assert_eq!(config.approval_profiles[0].api_key, secrets[1]);
}

#[test]
fn a_rotated_key_replaces_the_old_one() {
    let dir = temp_dir();
    let store = Store::open(&dir.0).unwrap();
    store
        .started("laptop", &start("chat", "sk-old", None))
        .unwrap();
    store
        .started("laptop", &start("chat", "sk-new", None))
        .unwrap();
    let keys = fs::read_to_string(dir.0.join("keys.json")).unwrap();
    assert!(
        keys.contains("sk-new") && !keys.contains("sk-old"),
        "{keys}"
    );
    // A configuration without keys gets the new one.
    let mut keyless = config("", "");
    keyless.approval_profiles = vec![model("openai::gpt", OPENAI, "")];
    store.configure("laptop", keyless).unwrap();
    let reviewer = &store.config_of("chat").unwrap().approval_profiles[0];
    assert_eq!(reviewer.api_key, "sk-new");
}

#[test]
fn a_key_goes_only_with_the_base_url_it_came_with() {
    let dir = temp_dir();
    let store = Store::open(&dir.0).unwrap();
    store
        .started("laptop", &start("chat", "sk-openai", None))
        .unwrap();
    let mut elsewhere = config("", "");
    elsewhere.approval_profiles = vec![
        model("openai::gpt", "https://proxy.example/v1", ""),
        model("openai::gpt", OPENAI, ""),
    ];
    store.configure("laptop", elsewhere).unwrap();
    let reviewers = store.config_of("chat").unwrap().approval_profiles;
    assert_eq!(reviewers[0].api_key, "", "another server's model");
    assert_eq!(reviewers[1].api_key, "sk-openai");
}

#[test]
fn the_files_are_this_users_alone() {
    let dir = temp_dir();
    let store = Store::open(&dir.0).unwrap();
    store
        .started("laptop", &start("chat", "sk-1", None))
        .unwrap();
    for name in ["keys.json", "state.json"] {
        let mode = fs::metadata(dir.0.join(name)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{name}");
    }
}

/// D16: a computer's configuration is its own chats', and another
/// computer's chat can't be started.
#[test]
fn each_computers_configuration_is_its_chats() {
    let dir = temp_dir();
    let store = Store::open(&dir.0).unwrap();
    let mut careful = config("", "");
    careful.permissions.edit_approval = ApprovalRule {
        required: true,
        by: Approver::User,
    };
    store.configure("laptop", careful.clone()).unwrap();
    store.configure("desk", config("", "")).unwrap();
    store.started("laptop", &start("a", "", None)).unwrap();
    store.started("desk", &start("b", "", None)).unwrap();
    assert!(
        store
            .config_of("a")
            .unwrap()
            .permissions
            .edit_approval
            .required
    );
    assert!(
        !store
            .config_of("b")
            .unwrap()
            .permissions
            .edit_approval
            .required
    );
    let refused = store.started("desk", &start("a", "", None)).unwrap_err();
    assert_eq!(refused.code, FailCode::NotOwner);
    assert_eq!(store.owner("a").as_deref(), Some("laptop"));
}

#[test]
fn the_store_reads_back_what_it_kept() {
    let dir = temp_dir();
    {
        let store = Store::open(&dir.0).unwrap();
        store
            .started(
                "laptop",
                &start("chat", "sk-chat", Some(config("sk-r", ""))),
            )
            .unwrap();
        store.run_started("chat", "run-1");
        store.ended("chat", Some("done"));
    }
    let state: State =
        serde_json::from_slice(&fs::read(dir.0.join("state.json")).unwrap()).unwrap();
    let chat = &state.chats["chat"];
    assert_eq!(chat.computer, "laptop");
    assert!(!chat.running);
    assert_eq!(chat.run_id.as_deref(), Some("run-1"));
    assert_eq!(chat.outcome.as_deref(), Some("done"));
    let settings = chat.start.as_ref().unwrap();
    assert_eq!(settings.profile.api_key, "", "no key in state.json");
    assert_eq!(settings.root.as_deref(), Some("/p"));
    let store = Store::open(&dir.0).unwrap();
    assert_eq!(store.owner("chat").as_deref(), Some("laptop"));
    let config = store.config_of("chat").unwrap();
    assert_eq!(
        config.approval_profiles[0].api_key, "sk-r",
        "from keys.json"
    );
}

/// 5.4: `running` goes from the run's `RunStarted` to its `RunEnded`, so a
/// start that never made a run leaves nothing to restart.
#[test]
fn a_chat_runs_from_its_runs_start_to_its_end() {
    let dir = temp_dir();
    let store = Store::open(&dir.0).unwrap();
    store.started("laptop", &start("chat", "", None)).unwrap();
    assert!(store.running().is_empty());
    store.run_started("chat", "run-1");
    store.run_started("elsewhere", "run-2");
    assert_eq!(store.running(), ["chat"]);
    assert_eq!(store.run_id("chat").as_deref(), Some("run-1"));
    store.ended("chat", Some("interrupted"));
    assert!(store.running().is_empty());
    let reopened = Store::open(&dir.0).unwrap();
    assert!(reopened.running().is_empty());
    let state: State =
        serde_json::from_slice(&fs::read(dir.0.join("state.json")).unwrap()).unwrap();
    assert_eq!(state.chats["chat"].outcome.as_deref(), Some("interrupted"));
}

#[test]
fn a_chat_is_restarted_at_most_twice_an_hour() {
    let dir = temp_dir();
    let store = Store::open(&dir.0).unwrap();
    store.started("laptop", &start("chat", "", None)).unwrap();
    let minute = 60 * 1000;
    assert!(store.count_restart("chat", 0));
    assert!(store.count_restart("chat", 10 * minute));
    assert!(!store.count_restart("chat", 20 * minute), "a third");
    // An hour after the first, it counts no more.
    assert!(store.count_restart("chat", 61 * minute));
    assert!(!store.count_restart("chat", 62 * minute));
    let reopened = Store::open(&dir.0).unwrap();
    assert!(!reopened.count_restart("chat", 63 * minute), "kept");
    assert!(!store.count_restart("unknown", 0));
}

/// A restart is rebuilt from the chat's settings and keys.json: nothing
/// of the message, which the database has, and no configuration, which
/// is its computer's.
#[test]
fn a_restart_is_its_last_start_again_with_its_key() {
    let dir = temp_dir();
    let first = start("chat", "sk-chat", Some(config("sk-r", "")));
    Store::open(&dir.0)
        .unwrap()
        .started("laptop", &first)
        .unwrap();
    let store = Store::open(&dir.0).unwrap();
    let restart = store
        .restart_request("chat", StartKind::ContinueReply)
        .unwrap();
    assert_eq!(restart.start, StartKind::ContinueReply);
    assert_eq!(restart.profile, first.profile, "with its key");
    assert_eq!(restart.mode, first.mode);
    assert_eq!(restart.root, first.root);
    assert_eq!(restart.current_user_content, None);
    assert_eq!(restart.config, None);
    assert!(store.restart_request("unknown", StartKind::Retry).is_none());
}

/// 6.5: a client passes over a result older than the newest it has.
#[test]
fn results_are_numbered_and_the_same_one_keeps_its_number() {
    let dir = temp_dir();
    let store = Store::open(&dir.0).unwrap();
    store.started("laptop", &start("chat", "", None)).unwrap();
    let result = |commit: &str, outcome: &str| ChatResult {
        dialog_id: "chat".into(),
        commit: commit.into(),
        seq: 0,
        outcome: outcome.into(),
        pending: None,
        marked: Vec::new(),
        unchecked: false,
    };

    assert_eq!(store.result_made("chat", result("c1", "done")).seq, 1);
    assert_eq!(store.result_made("chat", result("c1", "done")).seq, 1);
    let outcome = store.result_made("chat", result("c1", "verified"));
    assert_eq!(outcome.seq, 2, "the same commit after another run");
    assert_eq!(store.result_made("chat", result("c2", "verified")).seq, 3);

    let reopened = Store::open(&dir.0).unwrap();
    assert_eq!(reopened.result_of("chat").unwrap().seq, 3);
    assert_eq!(reopened.result_made("chat", result("c3", "done")).seq, 4);
}
