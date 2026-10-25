//! A project's services (D21): the databases, caches and queues its
//! compose file declares, which its own code connects to while it runs or
//! is tested. Docker Compose's file is where most projects keep them
//! (`compose.yaml`, `docker-compose.yml`), and Rovik reads it with Compose
//! itself (compose.rs): the services the file declares, which of them run
//! from an image of their own (a *backing* service) rather than the
//! project's code, and the ports each publishes.
//!
//! The agent hears of them twice. The project's section of its prompt
//! names them ([`prompt_line`]). And a command that couldn't connect to a
//! port one of them publishes, or looked one up by its name, is told which
//! service it needed and that it isn't running (agent_run/environment.rs):
//! the usual reason a dev server or a test suite that needs a database
//! fails. On a cloud host whose daemon runs each user's containers, the
//! daemon starts it ([`Runner`]), as it installs a missing toolchain, and
//! the command runs again. Elsewhere the agent asks the user: its commands
//! can't start containers, since the sandbox keeps the container engines'
//! sockets from them.

pub mod compose;
// A cloud host's daemon checks it, on Linux; its paths are Unix paths.
#[cfg(unix)]
pub mod policy;

use compose::{Model, Service};
use futures_util::future::BoxFuture;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// What starts a project's services: a cloud host's daemon.
pub trait Runner: Send + Sync {
    /// Starts the services `names` of `model` and what they depend on, for
    /// the chat `chat`, whose copy of the project `model` was read in: what
    /// runs now, in words for the agent, or why they couldn't be started.
    fn start(
        &self,
        chat: String,
        model: Arc<Model>,
        names: Vec<String>,
    ) -> BoxFuture<'static, Result<String, String>>;
}

/// What this machine does with a project's services.
#[derive(Clone)]
pub enum Host {
    /// A cloud host's daemon runs them.
    Runs(Arc<dyn Runner>),
    /// A cloud host that can't: why, and what its administrators can do.
    Cannot(String),
}

/// Set by a cloud host's daemon as it starts; None on the user's own
/// computer, where they run their services themselves.
static HOST: Mutex<Option<Host>> = Mutex::new(None);

/// What this machine does with the projects' services from now on: what a
/// cloud host's daemon calls once as it starts.
pub fn set_host(host: Host) {
    *HOST.lock().unwrap_or_else(|e| e.into_inner()) = Some(host);
}

/// What this machine does with the projects' services, where a daemon said.
pub fn host() -> Option<Host> {
    HOST.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Held by a test that sets what this machine does with the services
/// ([`set_host`]) or the Compose it reads them with: both are the whole
/// process's. Back to the user's own computer, and the Compose found, when
/// it's dropped.
#[cfg(test)]
pub(crate) fn test_lock() -> impl Drop {
    static LOCK: Mutex<()> = Mutex::new(());
    struct Held(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
    impl Drop for Held {
        fn drop(&mut self) {
            *HOST.lock().unwrap_or_else(|e| e.into_inner()) = None;
            compose::use_program(None);
        }
    }
    Held(LOCK.lock().unwrap_or_else(|e| e.into_inner()))
}

/// The most services the prompt names.
const MOST_NAMED: usize = 6;

/// The project's section of the prompt about its services: the backing
/// services of its compose files and where its code reaches them, and what
/// happens when one isn't running. None when it has none, or nothing here
/// can read its compose files. Blocking: Compose may run (compose.rs keeps
/// its reads).
pub fn prompt_line(root: &Path) -> Option<String> {
    if compose::folders(root).is_empty() {
        return None;
    }
    let program = compose::program()?;
    let models = compose::read_all(&program, root);
    prompt_line_for(&models, host().as_ref())
}

/// [`prompt_line`] for the compose files `models`, on a machine that does
/// `host` with them.
pub fn prompt_line_for(models: &[Arc<Model>], host: Option<&Host>) -> Option<String> {
    let mut named = Vec::new();
    for model in models {
        for service in model.backing() {
            named.push(format!("`{}` ({})", service.name, described(service)));
        }
    }
    if named.is_empty() {
        return None;
    }
    let more = named.len().saturating_sub(MOST_NAMED);
    named.truncate(MOST_NAMED);
    let mut list = named.join(", ");
    if more > 0 {
        list.push_str(&format!(" and {more} more"));
    }
    let files: Vec<&str> = models.iter().map(|m| m.file.as_str()).collect();
    let what = match host {
        Some(Host::Runs(_)) => {
            "They don't run until a command needs one: when a command can't connect to a \
             service's port, Rovik starts that service for this user's chats, as it installs \
             a missing toolchain, and runs the command again. Your commands have no container \
             engine here: a `docker compose up` has Rovik start what it names instead. Connect \
             to them at localhost, not by their names."
        }
        Some(Host::Cannot(_)) => {
            "This server can't run them: a command that needs one fails to connect, and the \
             message then says what its administrators can do. Tell the user."
        }
        None => {
            "Your commands can't start containers here: if the code needs a service that \
             isn't running, ask the user to start it (`docker compose up -d`)."
        }
    };
    Some(format!("Services ({}): {list}. {what}", files.join(", ")))
}

/// A service as the agent reads it: its image, and where it's reached.
fn described(service: &Service) -> String {
    let addresses = service.addresses();
    match addresses.is_empty() {
        true => format!("{}, no port published", service.image_name()),
        false => format!("{}, {}", service.image_name(), addresses.join(", ")),
    }
}

/// A service a command needed, and why Rovik thinks so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Needed {
    /// The service, by name.
    pub name: String,
    /// Each local port it was refused at that the service publishes.
    pub ports: Vec<u16>,
    /// It looked the service up by its name, which only the project's
    /// other containers can.
    pub by_name: bool,
}

/// The backing services of `model` a command's output shows it needed:
/// each service publishing one of `refused`, the ports a connection was
/// refused at, and each one named in `unresolved`, the host names it
/// couldn't look up. A service built from the project's code isn't one:
/// that's the agent's to run.
pub fn needed(model: &Model, refused: &[u16], unresolved: &[String]) -> Vec<Needed> {
    let mut needed: Vec<Needed> = Vec::new();
    let mut add = |service: &Service, port: Option<u16>, by_name: bool| {
        if !service.backing() {
            return;
        }
        let index = match needed.iter().position(|n| n.name == service.name) {
            Some(index) => index,
            None => {
                needed.push(Needed {
                    name: service.name.clone(),
                    ports: Vec::new(),
                    by_name: false,
                });
                needed.len() - 1
            }
        };
        let entry = &mut needed[index];
        if let Some(port) = port.filter(|p| !entry.ports.contains(p)) {
            entry.ports.push(port);
        }
        entry.by_name |= by_name;
    };
    for &port in refused {
        if let Some(service) = model.on_port(port) {
            add(service, Some(port), false);
        }
    }
    for name in unresolved {
        if let Some(service) = model.service(name) {
            add(service, None, true);
        }
    }
    needed
}

/// The services of `model` a command asked Compose to start by `names`
/// (`docker compose up -d db`): those of them that run from an image of
/// their own, or all such for no names. A service built from the project's
/// code is the agent's to run with its commands.
pub fn asked(model: &Model, names: &[String]) -> Vec<Needed> {
    model
        .backing()
        .filter(|service| names.is_empty() || names.contains(&service.name))
        .map(|service| Needed {
            name: service.name.clone(),
            ports: Vec::new(),
            by_name: false,
        })
        .collect()
}

/// What the agent is told, with the services Rovik started, after a
/// command of its asked a container engine to start them on a cloud host.
pub const NO_ENGINE: &str = "Your commands have no container engine on this server: Rovik \
                             runs the project's services itself and starts one whenever a \
                             command needs it, so they needn't be started with `docker` or \
                             `podman`.";

/// The services in `needed`, as the agent reads them: "the `db` service
/// (postgres:16-alpine) of compose.yaml".
fn named(model: &Model, needed: &[Needed]) -> String {
    let each: Vec<String> = needed
        .iter()
        .map(|n| {
            let image = model
                .service(&n.name)
                .map(|s| s.image_name().to_string())
                .unwrap_or_default();
            format!("`{}` ({image})", n.name)
        })
        .collect();
    let services = match each.as_slice() {
        [one] => format!("the {one} service"),
        [first @ .., last] => format!("the {} and {last} services", first.join(", ")),
        [] => "no service".to_string(),
    };
    format!("{services} of {}", model.file)
}

/// For a service the command looked up by its name: where the project's
/// code reaches it instead. None when it looked up none.
pub fn by_name_note(model: &Model, needed: &[Needed]) -> Option<String> {
    let looked_up: Vec<String> = needed
        .iter()
        .filter(|n| n.by_name)
        .filter_map(|n| {
            let address = model.service(&n.name)?.addresses().into_iter().next()?;
            Some(format!("`{}` at {address}", n.name))
        })
        .collect();
    (!looked_up.is_empty()).then(|| {
        format!(
            "The command looked a service up by its name, which only the project's other \
             containers can: its code reaches {} instead, so point it there (in its `.env` or \
             the variable it reads, such as DATABASE_URL) for this run.",
            looked_up.join(" and ")
        )
    })
}

/// The command `docker compose up -d` for `needed` of `model`, run in the
/// compose file's folder.
fn up_command(model: &Model, needed: &[Needed]) -> String {
    let names: Vec<&str> = needed.iter().map(|n| n.name.as_str()).collect();
    let up = format!("docker compose up -d {}", names.join(" "));
    match model.folder() {
        "" => up,
        folder => crate::agent_run::project::in_folder(folder, &up),
    }
}

/// Why Rovik didn't start the services a command needed.
#[derive(Clone, Copy, Debug)]
pub enum NotStarted<'a> {
    /// It's the user's own computer, where they run their services.
    Here,
    /// A cloud host that can't run them: why.
    Cannot(&'a str),
    /// The run is in Plan mode, which changes nothing.
    PlanMode,
}

/// What the agent is told when a command needed `needed` of `model` and
/// Rovik didn't start them, for the reason `why`.
pub fn not_started_note(model: &Model, needed: &[Needed], why: NotStarted) -> String {
    let services = named(model, needed);
    let what = match why {
        NotStarted::Here => format!(
            "The command needed {services}, which isn't running. Your commands can't start \
             containers here: ask the user to start it (`{}`), then run the command again.",
            up_command(model, needed)
        ),
        NotStarted::Cannot(why) => format!(
            "The command needed {services}, which isn't running: this server can't run the \
             project's services ({why}). Tell the user."
        ),
        NotStarted::PlanMode => format!(
            "The command needed {services}, which isn't running. Plan mode starts nothing: \
             Rovik starts it when a command needs it outside Plan mode."
        ),
    };
    match by_name_note(model, needed) {
        Some(note) => format!("{what} {note}"),
        None => what,
    }
}

/// What was done once Rovik started `needed` of `model`, as a sentence
/// without its full stop: `said` is the runner's own words (`at
/// localhost:5432`).
pub fn started(model: &Model, needed: &[Needed], said: &str) -> String {
    format!(
        "Rovik started {}, which the command needed ({said})",
        named(model, needed)
    )
}

/// What the agent is told when Rovik couldn't start `needed` of `model`.
pub fn not_started_because(model: &Model, needed: &[Needed], why: &str) -> String {
    format!(
        "The command needed {}, which isn't running, and Rovik couldn't start it: {why}",
        named(model, needed)
    )
}

/// The TCP ports the backing services of the project at `root` publish:
/// where a refused connection is a service's to explain, not the private
/// network's. Blocking, as [`prompt_line`].
pub fn published_ports(root: &Path) -> Vec<u16> {
    let Some(program) = compose::program() else {
        return Vec::new();
    };
    let mut ports: Vec<u16> = compose::read_all(&program, root)
        .iter()
        .flat_map(|model| {
            model
                .backing()
                .flat_map(|s| s.ports.iter().filter(|p| p.tcp).map(|p| p.published))
                .collect::<Vec<_>>()
        })
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

#[cfg(test)]
mod tests;
