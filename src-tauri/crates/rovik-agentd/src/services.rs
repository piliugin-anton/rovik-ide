//! This user's project services on a team server (D21): the databases,
//! caches and queues their projects' compose files declare. The daemon
//! starts one when a command of the agent's needs it (rovik_core::services,
//! agent_run/environment.rs), as it installs a missing toolchain, under the
//! user's own rootless Podman, with each port it publishes a port of the
//! user's private network rather than of the server.
//!
//!   podman.rs  Rovik's own Podman for this user: settings, storage, API
//!   copy.rs    copies of the project's files a service reads
//!   relay.rs   a service's ports, as ports of the user's network
//!
//! A project's services are one Compose project, `rovik-<project id>`,
//! shared by the user's chats of that project, as one computer's checkout
//! shares its database between branches: the first chat that needs a
//! service starts it from its own copy of the project, and the others find
//! it running (`--no-recreate`). What starts is checked first
//! (rovik_core::services::policy), and the project's files a service reads
//! are copies (copy.rs).

mod copy;
mod podman;
mod relay;

use crate::home::{self, Home};
use crate::installer;
use podman::Podman;
use relay::Relays;
use rovik_core::overlay::confine::network::Network;
use rovik_core::services::compose::Model;
use rovik_core::services::{self as core_services, policy, Host, Runner};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// How long starting services may take: pulling a database's image is
/// most of it.
const UP_WITHIN: Duration = Duration::from_secs(15 * 60);

/// How long Compose waits for a service to run, or to be healthy where
/// its file says how to tell.
const WAIT_SECONDS: &str = "300";

/// What runs a user's project services here, as `doctor` and the daemon
/// find it: Podman, the pinned Docker Compose, the user's subordinate ids.
pub struct Containers {
    /// What `podman --version` says, where Podman is installed.
    pub podman: Option<String>,
    /// Whether the pinned Docker Compose is installed.
    pub compose: bool,
    /// The subordinate uids and gids the user has (/etc/subuid, /etc/subgid).
    pub ids: (u64, u64),
}

impl Containers {
    /// This server's, for the user `user` with uid `uid`.
    pub fn here(user: &str, uid: u32) -> Containers {
        let podman = std::process::Command::new("podman")
            .arg("--version")
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
        let ids = |file: &str| {
            std::fs::read_to_string(file)
                .map(|text| installer::subids::ids_of(&text, user, uid))
                .unwrap_or(0)
        };
        Containers {
            podman,
            compose: rovik_core::pinned_compose::installed().is_some(),
            ids: (ids("/etc/subuid"), ids("/etc/subgid")),
        }
    }

    /// Podman's version, where it says one.
    pub fn version(&self) -> Option<(u32, u32)> {
        self.podman
            .as_deref()
            .and_then(installer::containers::podman_version)
    }

    /// What's missing for `user`'s services to run, each with how it's
    /// fixed: empty when nothing is.
    pub fn missing(&self, user: &str) -> Vec<String> {
        use installer::containers::MIN_PODMAN;
        use installer::subids::COUNT;
        use rovik_core::pinned_compose::{NAME, VERSION};
        let installer = installer::INSTALLED;
        let mut missing = Vec::new();
        match self.version() {
            None => missing.push(format!(
                "Podman isn't installed (sudo {installer} admin containers installs it)"
            )),
            Some((major, minor)) if (major, minor) < MIN_PODMAN => missing.push(format!(
                "Podman {major}.{minor} is older than the {}.{} Rovik needs (a newer release \
                 of the distribution has it)",
                MIN_PODMAN.0, MIN_PODMAN.1
            )),
            Some(_) => {}
        }
        if !self.compose {
            missing.push(format!(
                "{NAME} {VERSION} isn't installed (sudo {installer} admin containers installs it)"
            ));
        }
        let (uids, gids) = self.ids;
        if uids < COUNT || gids < COUNT {
            missing.push(format!(
                "{user} has {uids} subordinate uids and {gids} gids, not the {COUNT} of each \
                 their containers' users map to (sudo {installer} user start adds them)"
            ));
        }
        missing
    }
}

/// What this daemon does with its user's project services, said to the
/// core (`rovik_core::services::set_host`) as it starts: on a server Rovik
/// is installed on for everyone (`installed`), they run when Podman, the
/// pinned Compose, the user's ids and their private network (where the
/// services' ports go) are there, and the agent is told why not when one
/// isn't. Elsewhere (a daemon in development, WSL), the user runs them.
pub fn set_up(home: &Home, installed: bool) -> Option<Services> {
    if !installed || crate::network::in_wsl() {
        return None;
    }
    let cannot = |why: String| {
        log::warn!("the projects' services don't run for this user: {why}");
        core_services::set_host(Host::Cannot(why));
        None
    };
    let Some(network) = rovik_core::overlay::confine::network::current() else {
        return cannot(format!(
            "this user's commands have no private network here, where the services' ports \
             would be (sudo {} admin pasta installs what it needs)",
            installer::INSTALLED
        ));
    };
    let user = home::passwd().map(|p| p.name).unwrap_or_default();
    let missing = Containers::here(&user, home::uid()).missing(&user);
    if !missing.is_empty() {
        return cannot(missing.join("; "));
    }
    let Some(compose) = rovik_core::pinned_compose::installed() else {
        return cannot("Docker Compose isn't installed".into());
    };
    let services = Services::new(home.root.join("containers"), compose, network, Vec::new());
    core_services::set_host(Host::Runs(Arc::new(services.clone())));
    log::info!("the projects' services run under this user's Podman");
    Some(services)
}

/// This user's project services: what starts them, and their ports.
#[derive(Clone)]
pub struct Services {
    inner: Arc<Inner>,
}

struct Inner {
    podman: Arc<Podman>,
    relays: Relays,
    /// One start at a time: two chats needing the same database start it
    /// once.
    starting: tokio::sync::Mutex<()>,
}

impl Services {
    /// The services whose Podman keeps its state in `dir`, started with the
    /// Compose at `compose`, their ports in `network`, reading images in
    /// `image_stores` too.
    pub fn new(
        dir: PathBuf,
        compose: PathBuf,
        network: Arc<Network>,
        image_stores: Vec<PathBuf>,
    ) -> Services {
        Services {
            inner: Arc::new(Inner {
                podman: Arc::new(Podman::new(dir, compose, image_stores)),
                relays: Relays::new(network),
                starting: tokio::sync::Mutex::new(()),
            }),
        }
    }

    /// Every service stopped, as the daemon stops: a database ends cleanly
    /// rather than be killed with the unit.
    pub async fn stop_all(&self) {
        self.inner.podman.stop_all().await;
    }
}

impl Runner for Services {
    fn start(
        &self,
        chat: String,
        model: Arc<Model>,
        names: Vec<String>,
    ) -> futures_util::future::BoxFuture<'static, Result<String, String>> {
        let inner = self.inner.clone();
        Box::pin(async move {
            log::info!("services: {} for chat {chat}", names.join(", "));
            inner.start(&model, &names).await
        })
    }
}

impl Inner {
    /// `names` of `model`, with what they depend on, started and their
    /// ports relayed: where the code reaches them, or why they didn't
    /// start.
    async fn start(&self, model: &Model, names: &[String]) -> Result<String, String> {
        let _one = self.starting.lock().await;
        let root = model.root().to_path_buf();
        let plan = policy::check(model, names, &root)?;
        let project = project_name(&root, model);
        let running = self.podman.running(&project).await;
        let raw = with_copies(&self.podman, &project, model, &plan, &running).await?;
        let mut args: Vec<String> = [
            "-f",
            "-",
            "up",
            "--detach",
            "--wait",
            "--wait-timeout",
            WAIT_SECONDS,
            "--no-build",
            "--no-recreate",
            "--pull",
            "missing",
            "--quiet-pull",
        ]
        .map(String::from)
        .to_vec();
        args.extend(names.iter().cloned());
        let up = self
            .podman
            .compose(
                &project,
                &model.dir,
                &args,
                Some(raw.to_string()),
                UP_WITHIN,
            )
            .await;
        if let Err(why) = up {
            return Err(self.why_not(&project, &plan.services, &why).await);
        }
        let mut reached = Vec::new();
        let mut busy = Vec::new();
        for name in &plan.services {
            let Some(service) = model.service(name) else {
                continue;
            };
            for port in service.ports.iter().filter(|port| port.tcp) {
                let opened = self
                    .relays
                    .open(
                        self.podman.clone(),
                        &project,
                        name,
                        port.published,
                        port.target,
                    )
                    .await;
                match opened {
                    Ok(()) => reached.push((name.clone(), port.published)),
                    Err(why) => busy.push(why),
                }
            }
        }
        Ok(said(names, &reached, &busy))
    }

    /// Why `services` of `project` didn't start: what Compose said, and
    /// the last words of each service whose container doesn't run.
    async fn why_not(&self, project: &str, services: &[String], said: &str) -> String {
        let running = self.podman.running(project).await;
        let mut why = format!("Compose said: {said}");
        for service in services.iter().filter(|s| !running.contains(s)) {
            if let Some(logs) = self.podman.logs(project, service, 12).await {
                let logs: Vec<&str> = logs.lines().collect();
                why.push_str(&format!(
                    "; `{service}` said, last: {}",
                    logs[logs.len().saturating_sub(4)..].join(" / ")
                ));
            }
        }
        why
    }
}

/// `model`'s description to start as the Compose project `project` under
/// `podman`, `plan` checked: with no port published (each is a port of the
/// user's network instead) and each of the project's files it reads a
/// copy, kept in Podman's folder. A copy is made where there's none, and
/// again where its source changed and its service isn't `running`, so what
/// a running service wrote there stays.
async fn with_copies(
    podman: &Podman,
    project: &str,
    model: &Model,
    plan: &policy::Plan,
    running: &[String],
) -> Result<Value, String> {
    podman.prepare()?;
    let dir = podman.dir().join("sources").join(project);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut budget = copy::MOST_BYTES;
    let mut copies: HashMap<String, PathBuf> = HashMap::new();
    for source in &plan.sources {
        let at = dir.join(key_of(&source.what, &source.path));
        let data = at.join("data");
        let signed = at.join("signature");
        let path = source.path.clone();
        let now = tokio::task::spawn_blocking(move || copy::signature(&path))
            .await
            .map_err(|e| e.to_string())?;
        let in_use = match &source.service {
            Some(service) => running.contains(service),
            None => plan.services.iter().any(|s| running.contains(s)),
        };
        let current = std::fs::read_to_string(&signed).ok().as_deref() == Some(now.as_str());
        if !(data.exists() && (in_use || current)) {
            podman.remove(&data).await?;
            std::fs::create_dir_all(&at).map_err(|e| format!("{}: {e}", at.display()))?;
            let (root, path, to) = (plan.root.clone(), source.path.clone(), data.clone());
            let copied = tokio::task::spawn_blocking(move || {
                let mut budget = budget;
                copy::copy(&root, &path, &to, &mut budget).map(|()| budget)
            })
            .await
            .map_err(|e| e.to_string())?;
            budget = copied.map_err(|why| format!("{} for {}: {why}", model.file, source.what))?;
            std::fs::write(&signed, &now).map_err(|e| format!("{}: {e}", signed.display()))?;
        }
        copies.insert(source.named.clone(), data);
    }
    let mut raw = model.raw.clone();
    to_start(&mut raw, &copies);
    Ok(raw)
}

/// The Compose project a project's services are, from its chat's copy at
/// `root`: `rovik-` and the project's id, which its chats share, with the
/// compose file's folder for one one level down.
fn project_name(root: &Path, model: &Model) -> String {
    let id = rovik_core::chat_db::projects::folder_project_id(root).unwrap_or_else(|| {
        // A copy without the project's id file: its folder names it.
        key_of("project", root)
    });
    let mut name: String = format!("rovik-{}", id.to_ascii_lowercase())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(24)
        .collect::<String>()
        .trim_end_matches('-')
        .to_string();
    let folder: String = model
        .folder()
        .to_ascii_lowercase()
        .chars()
        .map(|c| match c.is_ascii_alphanumeric() {
            true => c,
            false => '-',
        })
        .collect();
    if !folder.is_empty() {
        name.push('-');
        name.push_str(&folder);
    }
    name
}

/// A short, stable name for `what` read at `path`: where its copy is kept.
fn key_of(what: &str, path: &Path) -> String {
    let mut hash = Sha256::new();
    hash.update(what.as_bytes());
    hash.update([0]);
    hash.update(path.as_os_str().as_encoded_bytes());
    hash.finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `raw`, Compose's description of a project, as Rovik starts it: no
/// service publishes a port, and each file of the project's it reads is
/// its copy (`copies`, by the path the description names). Nothing else
/// changes, and no key is added: Compose refuses one that's null.
fn to_start(raw: &mut Value, copies: &HashMap<String, PathBuf>) {
    let copy_of = |value: Option<&mut Value>| {
        let Some(value) = value else {
            return;
        };
        if let Some(copy) = value.as_str().and_then(|named| copies.get(named)) {
            *value = Value::String(copy.to_string_lossy().into_owned());
        }
    };
    let services = raw
        .get_mut("services")
        .and_then(Value::as_object_mut)
        .into_iter()
        .flat_map(|services| services.values_mut());
    for service in services.filter_map(Value::as_object_mut) {
        service.remove("ports");
        let mounts = service.get_mut("volumes").and_then(Value::as_array_mut);
        for mount in mounts.into_iter().flatten() {
            if mount.get("type").and_then(Value::as_str) == Some("bind") {
                copy_of(mount.get_mut("source"));
            }
        }
        let env_files = service.get_mut("env_file").and_then(Value::as_array_mut);
        for env_file in env_files.into_iter().flatten() {
            match env_file.is_string() {
                true => copy_of(Some(env_file)),
                false => copy_of(env_file.get_mut("path")),
            }
        }
    }
    for kind in ["secrets", "configs"] {
        let entries = raw
            .get_mut(kind)
            .and_then(Value::as_object_mut)
            .into_iter()
            .flat_map(|entries| entries.values_mut());
        for entry in entries {
            copy_of(entry.get_mut("file"));
        }
    }
}

/// What the agent is told of a start: where the services `asked` and what
/// they depend on are reached (`reached`: each service with a port), and
/// which ports couldn't be relayed and why (`busy`).
fn said(asked: &[String], reached: &[(String, u16)], busy: &[String]) -> String {
    let at = |wanted: bool| -> Vec<String> {
        reached
            .iter()
            .filter(|(service, _)| asked.contains(service) == wanted)
            .map(|(service, port)| match wanted && asked.len() == 1 {
                true => format!("localhost:{port}"),
                false => format!("`{service}` at localhost:{port}"),
            })
            .collect()
    };
    let (own, others) = (at(true), at(false));
    let mut words = match (own.is_empty(), asked.len()) {
        (true, _) => "it runs, publishing no port".to_string(),
        (false, 1) => format!("at {}", own.join(", ")),
        (false, _) => own.join(", "),
    };
    if !others.is_empty() {
        words.push_str(&format!(", with {}", others.join(", ")));
    }
    if !busy.is_empty() {
        words.push_str(&format!("; but {}", busy.join("; ")));
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_s_services_are_one_compose_project_named_for_it() {
        let dir = rovik_core::testing::temp_dir();
        let root = dir.0.as_path();
        std::fs::create_dir_all(root.join(".rovik")).unwrap();
        std::fs::write(
            root.join(".rovik/project.json"),
            r#"{"id": "4F0A9C2E-77aa-4b1e-9d1e-0123456789ab"}"#,
        )
        .unwrap();
        let printed = r#"{"name": "x", "services": {}}"#;
        let own = Model::parse(root, "compose.yaml", printed).unwrap();
        assert_eq!(project_name(root, &own), "rovik-4f0a9c2e-77aa-4b1e");
        let below = Model::parse(&root.join("Api_2"), "Api_2/compose.yaml", printed).unwrap();
        assert_eq!(project_name(root, &below), "rovik-4f0a9c2e-77aa-4b1e-api-2");
    }

    #[test]
    fn a_service_starts_with_no_port_published_and_reads_its_copies() {
        let mut raw = serde_json::json!({
            "services": {
                "db": {
                    "image": "postgres",
                    "ports": [{"target": 5432, "published": "5432"}],
                    "volumes": [
                        {"type": "bind", "source": "/w/db", "target": "/docker-entrypoint-initdb.d"},
                        {"type": "volume", "source": "data", "target": "/var/lib/postgresql/data"}
                    ],
                    "env_file": [{"path": "/w/db.env", "required": true}, "/w/more.env"]
                }
            },
            "secrets": {"pw": {"file": "/w/pw.txt"}}
        });
        let copies: HashMap<String, PathBuf> = [
            ("/w/db", "/c/1/data"),
            ("/w/db.env", "/c/2/data"),
            ("/w/more.env", "/c/3/data"),
            ("/w/pw.txt", "/c/4/data"),
        ]
        .into_iter()
        .map(|(named, copy)| (named.to_string(), PathBuf::from(copy)))
        .collect();
        to_start(&mut raw, &copies);
        let db = &raw["services"]["db"];
        assert!(db.get("ports").is_none(), "{db}");
        assert_eq!(db["volumes"][0]["source"], "/c/1/data");
        assert_eq!(
            db["volumes"][1]["source"], "data",
            "a named volume is the service's own"
        );
        assert_eq!(db["env_file"][0]["path"], "/c/2/data");
        assert_eq!(db["env_file"][1], "/c/3/data");
        assert_eq!(raw["secrets"]["pw"]["file"], "/c/4/data");

        // A service without env files or mounts gets none, nor a project
        // without secrets: Compose refuses a key that's null.
        let mut plain = serde_json::json!({"services": {"cache": {"image": "redis", "ports": []}}});
        to_start(&mut plain, &copies);
        assert_eq!(
            plain,
            serde_json::json!({"services": {"cache": {"image": "redis"}}})
        );
    }

    /// A service's files are copies, made from a project whose path goes
    /// through a link. While it runs, a change to them leaves its copy as
    /// it is, with what it wrote there; once it doesn't, the copy is made
    /// again.
    #[tokio::test]
    async fn a_services_files_are_copies_kept_while_it_runs() {
        let dir = rovik_core::testing::temp_dir();
        let real = dir.0.join("real");
        std::fs::create_dir_all(real.join("conf")).unwrap();
        std::fs::write(real.join("conf/redis.conf"), "port 6379\n").unwrap();
        let root = dir.0.join("project");
        std::os::unix::fs::symlink(&real, &root).unwrap();
        let printed = serde_json::json!({"name": "x", "services": {"cache": {
            "image": "redis",
            "volumes": [{"type": "bind", "source": root.join("conf"), "target": "/etc/redis"}]
        }}});
        let model = Model::parse(&root, "compose.yaml", &printed.to_string()).unwrap();
        let plan = policy::check(&model, &["cache".to_string()], &root).unwrap();
        let podman = Podman::new(
            dir.0.join("containers"),
            PathBuf::from("/bin/true"),
            Vec::new(),
        );
        let copied_at = |raw: Value| {
            PathBuf::from(
                raw["services"]["cache"]["volumes"][0]["source"]
                    .as_str()
                    .unwrap(),
            )
        };
        let conf = |at: &Path| std::fs::read_to_string(at.join("redis.conf")).unwrap();

        let raw = with_copies(&podman, "rovik-x", &model, &plan, &[]).await;
        let first = copied_at(raw.unwrap());
        let copies = dir.0.join("containers/sources/rovik-x");
        assert!(first.starts_with(&copies), "{}", first.display());
        assert_eq!(conf(&first), "port 6379\n");

        // It runs and writes there, and the project's file changes.
        std::fs::write(first.join("written"), "by the service").unwrap();
        std::fs::write(real.join("conf/redis.conf"), "port 6380\nsave \"\"\n").unwrap();
        let running = ["cache".to_string()];
        let raw = with_copies(&podman, "rovik-x", &model, &plan, &running).await;
        let kept = copied_at(raw.unwrap());
        assert_eq!(kept, first);
        assert_eq!(conf(&kept), "port 6379\n");
        assert!(kept.join("written").exists());

        // Stopped, it starts again from a new copy of the project's.
        let raw = with_copies(&podman, "rovik-x", &model, &plan, &[]).await;
        let fresh = copied_at(raw.unwrap());
        assert_eq!(conf(&fresh), "port 6380\nsave \"\"\n");
        assert!(!fresh.join("written").exists());
    }

    #[test]
    fn the_agent_is_told_where_the_services_are_reached() {
        let db = vec!["db".to_string()];
        assert_eq!(said(&db, &[("db".into(), 5432)], &[]), "at localhost:5432");
        assert_eq!(
            said(&db, &[("db".into(), 5432), ("cache".into(), 6379)], &[]),
            "at localhost:5432, with `cache` at localhost:6379"
        );
        assert_eq!(said(&db, &[], &[]), "it runs, publishing no port");
        assert_eq!(
            said(&db, &[], &["port 5432 of this user's network is held by `postgres` (pid 7)".into()]),
            "it runs, publishing no port; but port 5432 of this user's network is held by `postgres` (pid 7)"
        );
        let both = vec!["db".to_string(), "cache".to_string()];
        assert_eq!(
            said(&both, &[("db".into(), 5432), ("cache".into(), 6379)], &[]),
            "`db` at localhost:5432, `cache` at localhost:6379"
        );
    }

    /// A start for real: a project's Redis, its config file the project's,
    /// started for a chat under Podman, reached at localhost:6379 in the
    /// user's private network and not on the server's loopback. Run with
    /// `ROVIK_REAL_PODMAN=1`, as a user whose Podman runs rootless, where
    /// pasta is installed; `ROVIK_COMPOSE` names Compose where it isn't the
    /// pinned one.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn a_projects_service_is_started_and_reached_in_the_users_network() {
        use std::io::{Read, Write};
        if std::env::var("ROVIK_REAL_PODMAN").as_deref() != Ok("1") {
            return;
        }
        let dir = rovik_core::testing::temp_dir();
        let root = dir.0.join("project");
        std::fs::create_dir_all(root.join(".rovik")).unwrap();
        std::fs::write(
            root.join(".rovik/project.json"),
            r#"{"id": "e2e0e2e0-redis"}"#,
        )
        .unwrap();
        // A port of its own in the network, and one only the network has.
        std::fs::write(root.join("redis.conf"), "port 6379\nprotected-mode no\n").unwrap();
        std::fs::write(
            root.join("compose.yaml"),
            "services:\n  cache:\n    image: docker.io/library/redis:7-alpine\n    command: [\"redis-server\", \"/etc/redis/redis.conf\"]\n    volumes: [\"./redis.conf:/etc/redis/redis.conf:ro\"]\n    ports: [\"6379:6379\"]\n",
        )
        .unwrap();
        let compose = std::env::var_os("ROVIK_COMPOSE")
            .map(PathBuf::from)
            .or_else(rovik_core::pinned_compose::installed)
            .expect("Compose: ROVIK_COMPOSE or the pinned one");
        let program = rovik_core::services::compose::Program::Standalone(compose.clone());
        let model = rovik_core::services::compose::read(&program, &root, "")
            .unwrap()
            .unwrap();
        let network = Arc::new(
            Network::start(&dir.0.join("net"), Path::new("/bin/true")).expect("a private network"),
        );
        let services = Services::new(
            dir.0.join("containers"),
            compose,
            network.clone(),
            Vec::new(),
        );

        let said = services
            .inner
            .start(&model, &["cache".to_string()])
            .await
            .expect("Redis starts");
        assert_eq!(said, "at localhost:6379");

        let mut inside = network.connect(6379).expect("reached in the network");
        inside.write_all(b"PING\r\n").unwrap();
        let mut answer = [0u8; 7];
        inside.read_exact(&mut answer).unwrap();
        assert_eq!(&answer, b"+PONG\r\n");
        // Its config came from Rovik's copy, not the chat's file.
        let copies = dir.0.join("containers/sources");
        assert!(
            copies.read_dir().unwrap().next().is_some(),
            "a copy was made"
        );
        // Nothing of it on the server's loopback.
        let on_host = std::net::TcpStream::connect(("127.0.0.1", 6379));
        assert!(
            on_host.is_err(),
            "the server's own 6379 isn't the service's"
        );

        // Started again from the same chat, it's found running.
        let again = services.inner.start(&model, &["cache".to_string()]).await;
        assert_eq!(again.as_deref(), Ok("at localhost:6379"));
        services.stop_all().await;
        services
            .inner
            .podman
            .podman(&["rm", "--all", "--force"])
            .await
            .unwrap();
    }

    #[test]
    fn what_is_missing_says_how_it_is_fixed() {
        let ok = Containers {
            podman: Some("podman version 4.9.3".into()),
            compose: true,
            ids: (65536, 65536),
        };
        assert!(ok.missing("ann").is_empty());
        let none = Containers {
            podman: None,
            compose: false,
            ids: (0, 0),
        };
        let missing = none.missing("ann");
        assert_eq!(missing.len(), 3, "{missing:?}");
        assert!(
            missing[0].contains("admin containers installs it"),
            "{missing:?}"
        );
        assert!(missing[2].contains("user start adds them"), "{missing:?}");
    }
}
