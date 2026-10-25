//! A project's compose files, read by Docker Compose itself: the services
//! each declares, the image each runs, and the ports each publishes. Rovik
//! doesn't read the YAML on its own: Compose merges override files, reads
//! `.env` into `${VARIABLES}`, follows `extends` and `include` and turns
//! every short form into one long form, and anything less would name the
//! wrong port. `docker compose config --format json` prints what it read,
//! and that is the [`Model`].
//!
//! It's read with `--no-env-resolution`, which leaves each service's
//! `env_file` named, not read: the model never holds what's in those files,
//! the secrets a project keeps there among them, and a service's env files
//! can be checked before it starts (services/policy.rs).
//!
//! Compose is the program a cloud host pins (`rovik_core::pinned_compose`),
//! else the user's own (`docker compose`, `docker-compose`); without one, a
//! project's compose file is still found, but not read. What was read is
//! kept while the files Compose read in the folder stay as they were, for
//! a minute at most: an `extends` or `include` of a file elsewhere may
//! have changed meanwhile.

use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// The files Compose reads in a folder, in the order it looks for them; it
/// reads the first it finds.
pub const FILE_NAMES: &[&str] = &[
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];

/// What Compose reads beside a compose file without being told: its
/// override files, and the variables it puts into the file.
const BESIDE: &[&str] = &[
    "compose.override.yaml",
    "compose.override.yml",
    "docker-compose.override.yaml",
    "docker-compose.override.yml",
    ".env",
];

/// The longest a read is kept, whatever the files say.
const FRESH: Duration = Duration::from_secs(60);

/// How long Compose may take to read a project.
const READ_WITHIN: Duration = Duration::from_secs(15);

/// The compose file Compose would read in `dir`, by name, if there's one.
pub fn file_in(dir: &Path) -> Option<&'static str> {
    FILE_NAMES
        .iter()
        .copied()
        .find(|name| dir.join(name).is_file())
}

/// The folders of the project at `root` that have a compose file: its own
/// ("") first, then those one level down that may be a package of their
/// own (a monorepo's `api/`), as the ecosystems read them.
pub fn folders(root: &Path) -> Vec<String> {
    crate::ecosystems::Project::at(root)
        .folders
        .into_iter()
        .filter(|folder| file_in(&root.join(folder)).is_some())
        .collect()
}

/// A port a service publishes on the machine, and its port inside.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Port {
    /// Where the project's code reaches it: `localhost:<published>`.
    pub published: u16,
    /// The port the service listens on in its container.
    pub target: u16,
    /// TCP, not UDP.
    pub tcp: bool,
}

/// One service of a compose file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    /// The image it runs, where it names one.
    pub image: Option<String>,
    /// Whether it's built from a Dockerfile of the project's: the project's
    /// own code, which the agent runs with its commands, not a service of
    /// it.
    pub built: bool,
    /// What it publishes, in the file's order.
    pub ports: Vec<Port>,
    /// The services it needs started first.
    pub depends_on: Vec<String>,
}

impl Service {
    /// Whether it runs from an image of its own: a database, cache or
    /// queue the project's code connects to.
    pub fn backing(&self) -> bool {
        self.image.is_some() && !self.built
    }

    /// Its image, or "an image" where it names none.
    pub fn image_name(&self) -> &str {
        self.image.as_deref().unwrap_or("an image")
    }

    /// Where the project's code reaches it, as the agent reads it:
    /// `localhost:5432` (and the other TCP ports it publishes).
    pub fn addresses(&self) -> Vec<String> {
        self.ports
            .iter()
            .filter(|port| port.tcp)
            .map(|port| format!("localhost:{}", port.published))
            .collect()
    }
}

/// A compose file as Compose read it, with what's in it.
#[derive(Clone, Debug)]
pub struct Model {
    /// Compose's name for the project (`name:`, else its folder's).
    pub name: String,
    /// The folder it was read in.
    pub dir: PathBuf,
    /// The file Compose read there, for messages: `compose.yaml`, or
    /// `api/compose.yaml` one level down.
    pub file: String,
    /// Its services, by name.
    pub services: Vec<Service>,
    /// What Compose printed, whole: what a service is started from.
    pub raw: Value,
}

impl Model {
    /// The model in Compose's JSON `json`, read in `dir` (`file` names the
    /// compose file there, for messages).
    pub fn parse(dir: &Path, file: &str, json: &str) -> Result<Model, String> {
        let raw: Value = serde_json::from_str(json)
            .map_err(|e| format!("Compose's description of {file} isn't JSON: {e}"))?;
        let name = raw["name"].as_str().unwrap_or_default().to_string();
        let mut services: Vec<Service> = raw["services"]
            .as_object()
            .map(|services| {
                services
                    .iter()
                    .map(|(name, service)| Service {
                        name: name.clone(),
                        image: service["image"].as_str().map(str::to_string),
                        built: !service["build"].is_null(),
                        ports: ports(&service["ports"]),
                        depends_on: depends_on(&service["depends_on"]),
                    })
                    .collect()
            })
            .unwrap_or_default();
        services.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Model {
            name,
            dir: dir.to_path_buf(),
            file: file.to_string(),
            services,
            raw,
        })
    }

    /// The compose file's folder in the project: "" for the project's own.
    pub fn folder(&self) -> &str {
        self.file.rsplit_once('/').map_or("", |(folder, _)| folder)
    }

    /// The services that run from an image of their own.
    pub fn backing(&self) -> impl Iterator<Item = &Service> {
        self.services.iter().filter(|service| service.backing())
    }

    /// The service named `name`.
    pub fn service(&self, name: &str) -> Option<&Service> {
        self.services.iter().find(|service| service.name == name)
    }

    /// The service that publishes TCP `port`, if one does.
    pub fn on_port(&self, port: u16) -> Option<&Service> {
        self.services
            .iter()
            .find(|service| service.ports.iter().any(|p| p.tcp && p.published == port))
    }

    /// `names` and every service they depend on, each once, each after
    /// what it depends on: the order they start in.
    pub fn with_dependencies(&self, names: &[String]) -> Vec<String> {
        fn visit(model: &Model, name: &str, seen: &mut Vec<String>, order: &mut Vec<String>) {
            if seen.iter().any(|s| s == name) {
                return;
            }
            seen.push(name.to_string());
            if let Some(service) = model.service(name) {
                for dependency in &service.depends_on {
                    visit(model, dependency, seen, order);
                }
            }
            order.push(name.to_string());
        }
        let (mut seen, mut order) = (Vec::new(), Vec::new());
        for name in names {
            visit(self, name, &mut seen, &mut order);
        }
        order
    }
}

/// The ports Compose's long form lists: `published` is a string there, a
/// range already split into one entry each. One published on no port of the
/// machine (`"5432"` alone, a port Docker picks) isn't a place to reach.
fn ports(value: &Value) -> Vec<Port> {
    let Some(list) = value.as_array() else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|port| {
            let number = |v: &Value| match v {
                Value::String(s) => s.parse::<u16>().ok(),
                v => v.as_u64().and_then(|n| u16::try_from(n).ok()),
            };
            Some(Port {
                published: number(&port["published"])?,
                target: number(&port["target"])?,
                tcp: port["protocol"].as_str().unwrap_or("tcp") == "tcp",
            })
        })
        .collect()
}

/// The services a service depends on: Compose's long form is a map from
/// each to its condition.
fn depends_on(value: &Value) -> Vec<String> {
    let mut names: Vec<String> = match value {
        Value::Object(map) => map.keys().cloned().collect(),
        Value::Array(list) => list
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    names.sort();
    names
}

/// The Compose program a project is read and started with: a cloud host's
/// pinned one, else `docker compose` or `docker-compose` where the user has
/// one. Found once: none is installed while the app or the daemon runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Program {
    /// A program that is Compose itself: the pinned one, `docker-compose`.
    Standalone(PathBuf),
    /// Docker's own CLI, with Compose as its plugin: `docker compose`.
    Plugin(PathBuf),
}

impl Program {
    /// A command running it, with Compose's own arguments to come.
    pub fn command(&self) -> std::process::Command {
        match self {
            Program::Standalone(path) => crate::process::command(path),
            Program::Plugin(docker) => {
                let mut cmd = crate::process::command(docker);
                cmd.arg("compose");
                cmd
            }
        }
    }
}

/// The Compose a test reads with, in place of the one found.
#[cfg(test)]
static PROGRAM_FOR_TEST: Mutex<Option<Program>> = Mutex::new(None);

/// Reads compose files with `program` from now on, or with the one found
/// again (None): for a test, which holds `services::test_lock()`.
#[cfg(test)]
pub(crate) fn use_program(program: Option<Program>) {
    *PROGRAM_FOR_TEST.lock().unwrap_or_else(|e| e.into_inner()) = program;
}

/// The program this machine reads compose files with, if any.
pub fn program() -> Option<Program> {
    #[cfg(test)]
    if let Some(program) = PROGRAM_FOR_TEST
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        return Some(program);
    }
    found()
}

/// The program found on this machine, looked for once.
fn found() -> Option<Program> {
    static FOUND: LazyLock<Option<Program>> = LazyLock::new(|| {
        if let Some(pinned) = crate::pinned_compose::installed() {
            return Some(Program::Standalone(pinned));
        }
        if let Some(docker) = on_path("docker") {
            let mut cmd = crate::process::command(&docker);
            cmd.args(["compose", "version"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            crate::process::no_window(&mut cmd);
            if cmd.status().is_ok_and(|status| status.success()) {
                return Some(Program::Plugin(docker));
            }
        }
        on_path("docker-compose").map(Program::Standalone)
    });
    FOUND.clone()
}

/// Where `program` is on the PATH the app's programs get.
fn on_path(program: &str) -> Option<PathBuf> {
    let path = crate::process::child_var("PATH")?;
    let names: Vec<String> = match cfg!(windows) {
        true => vec![format!("{program}.exe")],
        false => vec![program.to_string()],
    };
    std::env::split_paths(&path)
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|file| file.is_file())
}

/// What a folder's read depends on: each file Compose reads there without
/// being told, with when it changed and its size.
type Fingerprint = Vec<(&'static str, Option<(SystemTime, u64)>)>;

fn fingerprint(dir: &Path) -> Fingerprint {
    FILE_NAMES
        .iter()
        .chain(BESIDE)
        .map(|name| {
            let meta = std::fs::metadata(dir.join(name)).ok();
            (
                *name,
                meta.and_then(|m| Some((m.modified().ok()?, m.len()))),
            )
        })
        .collect()
}

/// A folder's read, while it's fresh.
struct Read {
    at: Instant,
    fingerprint: Fingerprint,
    model: Result<Arc<Model>, String>,
}

static READS: LazyLock<Mutex<HashMap<PathBuf, Read>>> = LazyLock::new(Default::default);

/// The model of the compose file in `dir` of the project at `root`, read
/// with `program`, or why it couldn't be read; None when `dir` has none.
/// Blocking: Compose runs, unless an earlier read is still fresh.
pub fn read(program: &Program, root: &Path, folder: &str) -> Option<Result<Arc<Model>, String>> {
    let dir = root.join(folder);
    let name = file_in(&dir)?;
    let file = match folder {
        "" => name.to_string(),
        folder => format!("{folder}/{name}"),
    };
    let print = fingerprint(&dir);
    {
        let reads = READS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(read) = reads.get(&dir) {
            if read.at.elapsed() < FRESH && read.fingerprint == print {
                return Some(read.model.clone());
            }
        }
    }
    let model = run_config(program, &dir, &file).map(Arc::new);
    let mut reads = READS.lock().unwrap_or_else(|e| e.into_inner());
    reads.retain(|_, read| read.at.elapsed() < FRESH);
    reads.insert(
        dir,
        Read {
            at: Instant::now(),
            fingerprint: print,
            model: model.clone(),
        },
    );
    Some(model)
}

/// Every compose file of the project at `root` that `program` could read,
/// its own folder's first.
pub fn read_all(program: &Program, root: &Path) -> Vec<Arc<Model>> {
    folders(root)
        .iter()
        .filter_map(|folder| match read(program, root, folder)? {
            Ok(model) => Some(model),
            Err(why) => {
                log::info!("services: {why}");
                None
            }
        })
        .collect()
}

/// `config --format json --no-env-resolution` in `dir`: Compose with an
/// environment of its own (the PATH and the home folder), so a variable
/// the file names comes from the project's `.env`, not from the app's or
/// the daemon's environment, where a token may be.
fn run_config(program: &Program, dir: &Path, file: &str) -> Result<Model, String> {
    let mut cmd = program.command();
    cmd.args(["config", "--format", "json", "--no-env-resolution"])
        .current_dir(dir)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in ["PATH", "HOME", "SYSTEMROOT"] {
        if let Some(value) = crate::process::child_var(key) {
            cmd.env(key, value);
        }
    }
    crate::process::no_window(&mut cmd);
    let child = cmd
        .spawn()
        .map_err(|e| format!("Compose couldn't read {file}: {e}"))?;
    let out = wait_within(child, READ_WITHIN)
        .map_err(|e| format!("Compose couldn't read {file}: {e}"))?;
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr);
        return Err(format!("Compose couldn't read {file}: {}", said.trim()));
    }
    Model::parse(dir, file, &String::from_utf8_lossy(&out.stdout))
}

/// `child`'s output, once it has ended, or an error once `limit` passed,
/// the child killed.
fn wait_within(
    mut child: std::process::Child,
    limit: Duration,
) -> std::io::Result<std::process::Output> {
    use std::io::Read;
    // Its output is read meanwhile, so a big one doesn't fill the pipe and
    // hold it up.
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let readers = [
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(out) = stdout.as_mut() {
                let _ = out.read_to_end(&mut buf);
            }
            buf
        }),
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(err) = stderr.as_mut() {
                let _ = err.read_to_end(&mut buf);
            }
            buf
        }),
    ];
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("it took more than {} s", limit.as_secs()),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let [stdout, stderr] = readers.map(|reader| reader.join().unwrap_or_default());
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What Compose 5.6 prints for a project with a database published at
    /// a port from `.env`, a cache on two ports and one UDP, and the app
    /// built from the project's Dockerfile.
    pub(crate) const PRINTED: &str = r#"{
      "name": "shop",
      "services": {
        "web": {
          "build": {"context": "/p/shop", "dockerfile": "Dockerfile"},
          "depends_on": {"db": {"condition": "service_started", "required": true}},
          "ports": [{"mode": "ingress", "target": 3000, "published": "3000", "protocol": "tcp"}]
        },
        "db": {
          "image": "postgres:16-alpine",
          "env_file": [{"path": "/p/shop/db.env", "required": true}],
          "depends_on": {"cache": {"condition": "service_healthy", "required": true}},
          "ports": [{"mode": "ingress", "target": 5432, "published": "5433", "protocol": "tcp"}]
        },
        "cache": {
          "image": "redis:7-alpine",
          "ports": [
            {"mode": "ingress", "host_ip": "127.0.0.1", "target": 6379, "published": "6379", "protocol": "tcp"},
            {"mode": "ingress", "target": 8001, "published": "8001", "protocol": "tcp"},
            {"mode": "ingress", "target": 6000, "published": "6000", "protocol": "udp"},
            {"mode": "ingress", "target": 9000, "protocol": "tcp"}
          ]
        }
      }
    }"#;

    #[test]
    fn compose_s_description_gives_each_service_its_image_ports_and_dependencies() {
        let model = Model::parse(Path::new("/p/shop"), "compose.yaml", PRINTED).unwrap();
        assert_eq!(model.name, "shop");
        let names: Vec<&str> = model.services.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["cache", "db", "web"]);
        let db = model.service("db").unwrap();
        assert_eq!(db.image.as_deref(), Some("postgres:16-alpine"));
        assert!(db.backing());
        assert_eq!(
            db.ports,
            [Port {
                published: 5433,
                target: 5432,
                tcp: true
            }]
        );
        assert_eq!(db.depends_on, ["cache"]);
        let cache = model.service("cache").unwrap();
        assert_eq!(
            cache.addresses(),
            ["localhost:6379", "localhost:8001"],
            "TCP ports only, and not one Docker would pick"
        );
        let web = model.service("web").unwrap();
        assert!(web.built && !web.backing(), "the project's own code");
        let backing: Vec<&str> = model.backing().map(|s| s.name.as_str()).collect();
        assert_eq!(backing, ["cache", "db"]);
    }

    #[test]
    fn a_port_names_the_service_that_publishes_it() {
        let model = Model::parse(Path::new("/p/shop"), "compose.yaml", PRINTED).unwrap();
        assert_eq!(model.on_port(5433).map(|s| s.name.as_str()), Some("db"));
        assert_eq!(model.on_port(5432), None, "the port inside isn't published");
        assert_eq!(model.on_port(6000), None, "a UDP port isn't connected to");
        assert_eq!(model.on_port(3000).map(|s| s.name.as_str()), Some("web"));
    }

    #[test]
    fn services_start_after_what_they_depend_on() {
        let model = Model::parse(Path::new("/p/shop"), "compose.yaml", PRINTED).unwrap();
        assert_eq!(model.with_dependencies(&["db".into()]), ["cache", "db"]);
        assert_eq!(
            model.with_dependencies(&["web".into(), "cache".into()]),
            ["cache", "db", "web"]
        );
    }

    #[test]
    fn a_folder_with_a_compose_file_is_found_by_composes_own_names() {
        let dir = crate::testing::temp_dir();
        let root = dir.0.as_path();
        assert_eq!(file_in(root), None);
        std::fs::write(root.join("docker-compose.yml"), "services: {}\n").unwrap();
        assert_eq!(file_in(root), Some("docker-compose.yml"));
        std::fs::write(root.join("compose.yaml"), "services: {}\n").unwrap();
        assert_eq!(
            file_in(root),
            Some("compose.yaml"),
            "Compose's first choice"
        );
        std::fs::create_dir_all(root.join("api")).unwrap();
        std::fs::write(root.join("api/compose.yml"), "services: {}\n").unwrap();
        std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
        std::fs::write(root.join("node_modules/compose.yaml"), "services: {}\n").unwrap();
        assert_eq!(folders(root), ["", "api"]);
    }

    /// With the pinned Compose (or the user's), a real project is read as
    /// Compose reads it: `.env` in the ports, the env file named, not read.
    #[test]
    fn compose_itself_reads_the_project() {
        let _lock = crate::services::test_lock();
        let Some(program) = program() else {
            eprintln!("no Compose here: skipped");
            return;
        };
        let dir = crate::testing::temp_dir();
        let root = dir.0.as_path();
        std::fs::write(root.join(".env"), "DB_PORT=5433\n").unwrap();
        std::fs::write(root.join("db.env"), "POSTGRES_PASSWORD=hunter2\n").unwrap();
        std::fs::write(
            root.join("compose.yaml"),
            "name: shop\nservices:\n  db:\n    image: postgres:16-alpine\n    env_file: [db.env]\n    ports: [\"${DB_PORT:-5432}:5432\"]\n",
        )
        .unwrap();
        let model = read(&program, root, "").unwrap().unwrap();
        assert_eq!(model.service("db").unwrap().ports[0].published, 5433);
        assert!(
            !model.raw.to_string().contains("hunter2"),
            "the env file's values stay out of the model"
        );
        // Read again while nothing changed: the same read.
        let again = read(&program, root, "").unwrap().unwrap();
        assert!(Arc::ptr_eq(&model, &again));
        // A changed `.env` is read again.
        std::fs::write(root.join(".env"), "DB_PORT=15434\n").unwrap();
        let changed = read(&program, root, "").unwrap().unwrap();
        assert_eq!(changed.service("db").unwrap().ports[0].published, 15434);
    }
}
