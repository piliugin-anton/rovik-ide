//! What a project's service may do when a cloud host's daemon starts it
//! (D21). Rovik starts it from the compose file in the chat's copy of the
//! project, which the agent writes: the file mustn't become a way out of
//! the sandbox the agent's commands run in. The service's containers run
//! rootless, as the user, so none of them can do what the user couldn't;
//! but the user can read their own SSH keys and Rovik's API keys, which the
//! sandbox hides from the agent. So a service starts only when what it
//! would reach of the server is its own:
//!
//! - **Files**: a bind mount, an `env_file`, and a secret's or config's
//!   `file` must be inside the chat's copy of the project. Each is copied
//!   into a folder of the daemon's before the service starts (the daemon's
//!   services/copy.rs), so a link put in its place meanwhile reaches
//!   nothing. A named volume is the service's own.
//! - **No host devices or namespaces**: no `privileged`, no `devices` or
//!   GPUs, no `network_mode`, `pid`, `ipc` or `uts` of the host or of a
//!   container outside the project, no user namespace or cgroup of its
//!   choosing, no `security_opt` but `no-new-privileges`. On the host's
//!   network a service would listen on the server's own ports, every
//!   user's, rather than in the user's private network.
//! - **An image of its own**: a service built from the project's
//!   Dockerfile is the project's own code, which the agent runs with its
//!   commands.
//!
//! Its published ports aren't published on the server: each is made a port
//! of the user's private network instead (the daemon's services/relay.rs).

use super::compose::Model;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// A file or folder of the project's that a service reads, as a service
/// gets it: a copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// The service that reads it; None for a secret or a config, which any
    /// of the services starting may.
    pub service: Option<String>,
    /// How the model names it: what the daemon replaces with its copy.
    pub named: String,
    /// Where it is in the chat's copy of the project, its links followed.
    pub path: PathBuf,
    /// What the model says it is, for messages: "the bind mount at
    /// /docker-entrypoint-initdb.d".
    pub what: String,
}

/// What may start: the services, in the order they start, and the files of
/// the project's they read, each to be copied.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// The chat's copy of the project, its links followed: where each
    /// source's `path` is.
    pub root: PathBuf,
    /// The services, each after those it depends on.
    pub services: Vec<String>,
    /// The project's files they read.
    pub sources: Vec<Source>,
}

/// Whether `names` of `model`, with what they depend on, may start for a
/// chat whose copy of the project is `root`: the plan, or why not, in words
/// for the agent.
pub fn check(model: &Model, names: &[String], root: &Path) -> Result<Plan, String> {
    let real = root
        .canonicalize()
        .map_err(|e| format!("{} can't be checked: {}: {e}", model.file, root.display()))?;
    let root = real.as_path();
    let services = model.with_dependencies(names);
    let mut plan = Plan {
        root: real.clone(),
        services: services.clone(),
        sources: Vec::new(),
    };
    for name in &services {
        let service = &model.raw["services"][name.as_str()];
        if service.is_null() {
            return Err(format!("{} names no service `{name}`", model.file));
        }
        let refused = |why: String| format!("`{name}` in {} {why}", model.file);
        if !service["build"].is_null() {
            return Err(refused(
                "is built from the project's own Dockerfile: that's the project's code, which \
                 the agent runs with its commands, not a service Rovik starts"
                    .into(),
            ));
        }
        if service["image"].as_str().is_none() {
            return Err(refused("names no image to run".into()));
        }
        if let Some(why) = host_access(service, &model.services_named()) {
            return Err(refused(why));
        }
        for (path, what) in files_of(service) {
            let mut source = inside(path, what, root).map_err(&refused)?;
            source.service = Some(name.clone());
            plan.sources.push(source);
        }
    }
    for (kind, value) in [
        ("secrets", &model.raw["secrets"]),
        ("configs", &model.raw["configs"]),
    ] {
        for (key, entry) in value.as_object().into_iter().flatten() {
            let used = services
                .iter()
                .any(|name| uses(&model.raw["services"][name.as_str()][kind], key));
            let Some(file) = entry["file"].as_str().filter(|_| used) else {
                continue;
            };
            let what = format!("the file of its {} `{key}`", &kind[..kind.len() - 1]);
            let source =
                inside(file.into(), what, root).map_err(|why| format!("{} {why}", model.file))?;
            plan.sources.push(source);
        }
    }
    for (key, volume) in model.raw["volumes"].as_object().into_iter().flatten() {
        let opts = &volume["driver_opts"];
        if opts.as_object().is_some_and(|o| !o.is_empty()) || !volume["driver"].is_null() {
            let used = services.iter().any(|name| {
                model.raw["services"][name.as_str()]["volumes"]
                    .as_array()
                    .is_some_and(|mounts| mounts.iter().any(|m| m["source"] == key.as_str()))
            });
            if used {
                return Err(format!(
                    "{}: the volume `{key}` names a driver or its options, which can mount \
                     anything of the server's: Rovik starts services with plain named volumes",
                    model.file
                ));
            }
        }
    }
    Ok(plan)
}

/// What of the host `service` asks for that Rovik doesn't give: None when
/// nothing. `project` names the compose file's services, which a service
/// may share a namespace with.
fn host_access(service: &Value, project: &[&str]) -> Option<String> {
    let set = |key: &str| match &service[key] {
        Value::Null => false,
        Value::Bool(on) => *on,
        Value::String(s) => !s.is_empty(),
        Value::Array(list) => !list.is_empty(),
        Value::Object(map) => !map.is_empty(),
        Value::Number(_) => true,
    };
    for key in ["privileged", "devices", "device_cgroup_rules", "gpus"] {
        if set(key) {
            return Some(format!(
                "asks for `{key}`: a service gets none of the server's devices"
            ));
        }
    }
    for key in [
        "userns_mode",
        "cgroup",
        "cgroup_parent",
        "runtime",
        "isolation",
        "storage_opt",
        "credential_spec",
    ] {
        if set(key) {
            return Some(format!("sets `{key}`, which Rovik leaves to Podman"));
        }
    }
    let reservations = &service["deploy"]["resources"]["reservations"]["devices"];
    if reservations.as_array().is_some_and(|d| !d.is_empty()) {
        return Some("reserves devices: a service gets none of the server's".into());
    }
    let own = |mode: &str| {
        mode.is_empty()
            || mode == "private"
            || mode == "shareable"
            || mode
                .strip_prefix("service:")
                .is_some_and(|other| project.contains(&other))
    };
    for key in ["network_mode", "pid", "ipc", "uts"] {
        let Some(mode) = service[key].as_str() else {
            continue;
        };
        let allowed =
            own(mode) || (key == "network_mode" && matches!(mode, "none" | "bridge" | "default"));
        if !allowed {
            return Some(format!(
                "sets `{key}: {mode}`: a service shares a namespace only with the project's other \
                 services, and its network is the user's own (on the host's, it would listen on \
                 every user's ports)"
            ));
        }
    }
    for option in service["security_opt"].as_array().into_iter().flatten() {
        let option = option.as_str().unwrap_or_default();
        if !option.starts_with("no-new-privileges") {
            return Some(format!(
                "sets `security_opt: {option}`: a service keeps the confinement Podman gives it"
            ));
        }
    }
    for from in service["volumes_from"].as_array().into_iter().flatten() {
        let from = from.as_str().unwrap_or_default();
        let named = from.split(':').next().unwrap_or_default();
        let other = from.strip_prefix("service:").unwrap_or(named);
        if from.starts_with("container:") || !project.contains(&other) {
            return Some(format!(
                "mounts the volumes of `{from}`, which isn't one of the project's services"
            ));
        }
    }
    for mount in service["volumes"].as_array().into_iter().flatten() {
        let kind = mount["type"].as_str().unwrap_or("volume");
        if !matches!(kind, "bind" | "volume" | "tmpfs" | "image") {
            return Some(format!(
                "mounts a `{kind}`, which Rovik doesn't give a service"
            ));
        }
    }
    None
}

/// The project's files `service` reads: its bind mounts' sources and its
/// env files, as the model names them, each with what it is.
fn files_of(service: &Value) -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    for mount in service["volumes"].as_array().into_iter().flatten() {
        if mount["type"] == "bind" {
            if let Some(source) = mount["source"].as_str() {
                let target = mount["target"].as_str().unwrap_or("?");
                files.push((source.into(), format!("its bind mount at {target}")));
            }
        }
    }
    for env_file in service["env_file"].as_array().into_iter().flatten() {
        let path = env_file["path"].as_str().or_else(|| env_file.as_str());
        if let Some(path) = path {
            files.push((path.into(), "its env file".to_string()));
        }
    }
    files
}

/// Whether a service's `secrets` or `configs` list (`list`) uses `key`:
/// by its short name, or as the long form's `source`.
fn uses(list: &Value, key: &str) -> bool {
    list.as_array()
        .into_iter()
        .flatten()
        .any(|entry| entry.as_str() == Some(key) || entry["source"].as_str() == Some(key))
}

/// `path`, as a source a service may read, when it's in the chat's copy of
/// the project at `root` (whose links are followed already), its own links
/// followed; why not, when it isn't. One that isn't there yet is made empty
/// when it's copied, as Compose makes a bind mount's missing folder: it's
/// placed by the nearest folder above it that is there. A `..` in it is
/// refused outright: with no file there to follow, it can't be checked.
fn inside(path: PathBuf, what: String, root: &Path) -> Result<Source, String> {
    let outside = || {
        format!(
            "reads {} through {what}, which is outside the chat's copy of the project: a \
             service reads only the project's own files",
            path.display()
        )
    };
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(outside());
    }
    let resolved = path
        .ancestors()
        .find_map(|existing| {
            let real = existing.canonicalize().ok()?;
            let rest = path.strip_prefix(existing).ok()?;
            Some(real.join(rest))
        })
        .ok_or_else(outside)?;
    if !resolved.starts_with(root) {
        return Err(outside());
    }
    Ok(Source {
        service: None,
        named: path.to_string_lossy().into_owned(),
        path: resolved,
        what,
    })
}

impl Model {
    /// The names of its services.
    fn services_named(&self) -> Vec<&str> {
        self.services.iter().map(|s| s.name.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project in a temporary folder whose compose file Compose read as
    /// `services`, its other top-level keys `extra`.
    fn model_in(root: &Path, services: Value, extra: Value) -> Model {
        let mut raw = serde_json::json!({ "name": "shop", "services": services });
        for (key, value) in extra.as_object().into_iter().flatten() {
            raw[key] = value.clone();
        }
        Model::parse(root, "compose.yaml", &raw.to_string()).unwrap()
    }

    fn project() -> crate::testing::TempDir {
        let dir = crate::testing::temp_dir();
        std::fs::create_dir_all(dir.0.join("db")).unwrap();
        std::fs::write(dir.0.join("db/init.sql"), "create table t ();").unwrap();
        std::fs::write(dir.0.join("db.env"), "POSTGRES_PASSWORD=x").unwrap();
        dir
    }

    #[test]
    fn a_service_reading_the_projects_files_starts_with_copies_of_them() {
        let dir = project();
        let root = dir.0.as_path();
        let model = model_in(
            root,
            serde_json::json!({
                "db": {
                    "image": "postgres:16",
                    "env_file": [{"path": root.join("db.env"), "required": true}],
                    "volumes": [
                        {"type": "bind", "source": root.join("db"), "target": "/docker-entrypoint-initdb.d"},
                        {"type": "volume", "source": "data", "target": "/var/lib/postgresql/data"}
                    ],
                    "secrets": ["pw"],
                    "depends_on": {"cache": {"condition": "service_started"}}
                },
                "cache": {"image": "redis:7"},
                "web": {"build": {"context": root}}
            }),
            serde_json::json!({
                "volumes": {"data": {}},
                "secrets": {"pw": {"file": root.join("db/init.sql")}, "unused": {"file": "/etc/passwd"}}
            }),
        );
        let plan = check(&model, &["db".into()], root).unwrap();
        assert_eq!(plan.services, ["cache", "db"]);
        let real = root.canonicalize().unwrap();
        assert_eq!(plan.root, real);
        let sources: Vec<(PathBuf, &str)> = plan
            .sources
            .iter()
            .map(|s| (s.path.clone(), s.what.as_str()))
            .collect();
        assert_eq!(
            sources,
            [
                (
                    real.join("db"),
                    "its bind mount at /docker-entrypoint-initdb.d"
                ),
                (real.join("db.env"), "its env file"),
                (real.join("db/init.sql"), "the file of its secret `pw`"),
            ],
            "a secret no starting service uses isn't read"
        );
    }

    #[test]
    fn a_file_outside_the_project_is_refused_however_its_named() {
        let dir = project();
        let root = dir.0.as_path();
        let outside = crate::testing::temp_dir();
        std::fs::write(outside.0.join("id_rsa"), "secret").unwrap();
        // A link in the project to a file outside it.
        std::os::unix::fs::symlink(outside.0.join("id_rsa"), root.join("key")).unwrap();
        for service in [
            serde_json::json!({"image": "nginx", "volumes": [{"type": "bind", "source": "/home", "target": "/h"}]}),
            serde_json::json!({"image": "nginx", "volumes": [{"type": "bind", "source": root.join("key"), "target": "/k"}]}),
            serde_json::json!({"image": "nginx", "env_file": [{"path": root.join("../x/../../etc/passwd")}]}),
        ] {
            let model = model_in(root, serde_json::json!({ "web": service }), Value::Null);
            let why = check(&model, &["web".into()], root).unwrap_err();
            assert!(
                why.contains("outside the chat's copy of the project"),
                "{why}"
            );
        }
        let model = model_in(
            root,
            serde_json::json!({"web": {"image": "nginx", "secrets": [{"source": "s"}]}}),
            serde_json::json!({"secrets": {"s": {"file": outside.0.join("id_rsa")}}}),
        );
        let why = check(&model, &["web".into()], root).unwrap_err();
        assert!(why.contains("the file of its secret `s`"), "{why}");
    }

    /// A project whose path goes through a link (a home folder kept
    /// elsewhere) is checked where it is, and the plan says where; the
    /// model's own names for its files stay what they were.
    #[test]
    fn a_project_reached_through_a_link_is_checked_where_it_is() {
        let dir = project();
        let elsewhere = crate::testing::temp_dir();
        let root = elsewhere.0.join("project");
        std::os::unix::fs::symlink(&dir.0, &root).unwrap();
        let model = model_in(
            &root,
            serde_json::json!({"db": {"image": "postgres", "env_file": [{"path": root.join("db.env")}]}}),
            Value::Null,
        );
        let plan = check(&model, &["db".into()], &root).unwrap();
        let real = dir.0.canonicalize().unwrap();
        assert_eq!(plan.root, real);
        assert_eq!(plan.sources[0].path, real.join("db.env"));
        assert_eq!(
            Path::new(&plan.sources[0].named),
            root.join("db.env"),
            "the name the daemon replaces with the copy"
        );
    }

    #[test]
    fn a_source_not_there_yet_is_placed_by_the_folder_above_it() {
        let dir = project();
        let root = dir.0.as_path();
        let real = root.canonicalize().unwrap();
        let mount = |source: PathBuf| {
            model_in(
                root,
                serde_json::json!({"db": {"image": "postgres", "volumes": [
                    {"type": "bind", "source": source, "target": "/var/lib/postgresql/data"}
                ]}}),
                Value::Null,
            )
        };
        let plan = check(&mount(root.join("data/pg")), &["db".into()], root).unwrap();
        assert_eq!(plan.sources[0].path, real.join("data/pg"));
        // Through `..` it can't be checked, there or not.
        for sneaky in [root.join("data/../../x"), root.join("db/../db")] {
            let why = check(&mount(sneaky), &["db".into()], root).unwrap_err();
            assert!(why.contains("outside the chat's copy"), "{why}");
        }
    }

    #[test]
    fn a_service_gets_none_of_the_hosts_devices_or_namespaces() {
        let dir = project();
        let root = dir.0.as_path();
        for (key, value, said) in [
            ("privileged", serde_json::json!(true), "`privileged`"),
            (
                "devices",
                serde_json::json!(["/dev/kvm:/dev/kvm"]),
                "`devices`",
            ),
            (
                "network_mode",
                serde_json::json!("host"),
                "`network_mode: host`",
            ),
            (
                "network_mode",
                serde_json::json!("container:other"),
                "`network_mode: container:other`",
            ),
            ("pid", serde_json::json!("host"), "`pid: host`"),
            ("ipc", serde_json::json!("host"), "`ipc: host`"),
            ("userns_mode", serde_json::json!("host"), "`userns_mode`"),
            (
                "security_opt",
                serde_json::json!(["seccomp:unconfined"]),
                "`security_opt: seccomp:unconfined`",
            ),
            (
                "volumes_from",
                serde_json::json!(["container:x"]),
                "`container:x`",
            ),
            (
                "cgroup_parent",
                serde_json::json!("system.slice"),
                "`cgroup_parent`",
            ),
        ] {
            let mut service = serde_json::json!({"image": "busybox"});
            service[key] = value;
            let model = model_in(
                root,
                serde_json::json!({"s": service, "db": {"image": "postgres"}}),
                Value::Null,
            );
            let why = check(&model, &["s".into()], root).unwrap_err();
            assert!(why.starts_with("`s` in compose.yaml"), "{why}");
            assert!(why.contains(said), "{key}: {why}");
        }
        // What stays within the project is fine.
        for (key, value) in [
            ("network_mode", serde_json::json!("service:db")),
            ("network_mode", serde_json::json!("none")),
            ("pid", serde_json::json!("service:db")),
            (
                "security_opt",
                serde_json::json!(["no-new-privileges:true"]),
            ),
            ("volumes_from", serde_json::json!(["db:ro"])),
            ("cap_add", serde_json::json!(["NET_ADMIN"])),
        ] {
            let mut service = serde_json::json!({"image": "busybox"});
            service[key] = value;
            let model = model_in(
                root,
                serde_json::json!({"s": service, "db": {"image": "postgres"}}),
                Value::Null,
            );
            assert!(check(&model, &["s".into()], root).is_ok(), "{key}");
        }
    }

    #[test]
    fn the_projects_own_code_and_a_volume_with_a_driver_are_not_started() {
        let dir = project();
        let root = dir.0.as_path();
        let model = model_in(
            root,
            serde_json::json!({
                "db": {"image": "postgres", "depends_on": {"migrate": {"condition": "service_completed_successfully"}}},
                "migrate": {"build": {"context": root}}
            }),
            Value::Null,
        );
        let why = check(&model, &["db".into()], root).unwrap_err();
        assert!(
            why.starts_with("`migrate` in compose.yaml is built from the project's own Dockerfile"),
            "{why}"
        );

        let model = model_in(
            root,
            serde_json::json!({"db": {"image": "postgres", "volumes": [{"type": "volume", "source": "data", "target": "/d"}]}}),
            serde_json::json!({"volumes": {"data": {"driver_opts": {"type": "none", "o": "bind", "device": "/home"}}}}),
        );
        let why = check(&model, &["db".into()], root).unwrap_err();
        assert!(
            why.contains("the volume `data` names a driver or its options"),
            "{why}"
        );
    }
}
