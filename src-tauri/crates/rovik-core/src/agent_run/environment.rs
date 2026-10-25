//! What a command needs of where it runs, made ready around it: the same
//! for every command the agent runs, Rovik's `bash` (tools.rs), a
//! connector's harness running its own shell (its hooks,
//! connectors/claude_code.rs), and Prepare's installs (prepare.rs).
//!
//! - [`before`] a command, the programs it starts that an ecosystem brings
//!   (ecosystems/) and that aren't there are made available; before a
//!   harness's own shell's, never in Plan mode ([`before_harness_command`]).
//! - [`after`] it failed for want of a program or of a package, the
//!   program is made available, or the project's dependencies installed;
//!   the caller runs the command again, or tells the model to.
//! - [`after`] it failed to reach one of the project's services (a port
//!   its compose file publishes refused the connection, or a service's
//!   name couldn't be looked up), the service is started where this
//!   machine runs them (a cloud host's daemon, services/), or the model is
//!   told it isn't running and what to do.
//!
//! A program is made available on a cloud host by installing, as root, the
//! Node or Rust version the project runs (toolchains::ensure), and Yarn or
//! pnpm through Corepack on that Node (toolchains::add_shim); anywhere, by
//! reading the login shell's PATH again, for one installed since the app
//! started. That takes a login shell's start, so before the agent's
//! commands only the host's part is done ([`Look::Host`]), and the rest
//! once one failed. Rovik installs no toolchain on the user's own
//! computer: the model is told to tell the user. Dependencies install as
//! Prepare's do, once in a while at most for each, never in Plan mode, and
//! only where the user's permissions would run that install without
//! asking.

use super::command_output::{environment_failure, program_not_found};
use super::permissions::{self, Verdict};
use super::StopSignal;
use crate::ecosystems::{self, Ecosystem, Install};
use crate::services::compose::Model;
use crate::services::{self, Needed, NotStarted};
use crate::toolchains::{self, Want};
use crate::Core;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The root command an administrator installs a toolchain with.
const ROOT_COMMAND: &str = "/opt/rovik/bin/rovik-agentd";

/// How long an install that ran for a chat isn't run for it again: one that
/// failed would fail the same way on every command after it.
const INSTALL_AGAIN_AFTER: Duration = Duration::from_secs(10 * 60);

/// Where a command runs, and for whom.
#[derive(Clone)]
pub struct Context {
    /// The core: the chat's settings, its events.
    pub core: Core,
    /// The chat it's for: its settings, its copy of the project, where an
    /// install's output is saved.
    pub chat: String,
    /// The folder it runs in: the chat's copy of the project, or the
    /// project.
    pub root: String,
    /// Stops an install with the run.
    pub stop: Arc<StopSignal>,
    /// Says what's being installed while it is; None clears it.
    pub note: Arc<dyn Fn(Option<String>) + Send + Sync>,
    /// The run is in Plan mode, which changes nothing: no dependencies
    /// are installed, nor anything before a command a harness's own shell
    /// runs ([`before_harness_command`]).
    pub plan_mode: bool,
}

/// What was made ready for a command, for the model.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Ready {
    /// What's there now that wasn't, each a sentence without its full stop.
    pub done: Vec<String>,
    /// What still isn't, and what can be done about it, each a sentence.
    pub missing: Vec<String>,
}

impl Ready {
    /// Whether anything is there now that wasn't: the command may pass if
    /// it runs again.
    pub fn changed(&self) -> bool {
        !self.done.is_empty()
    }

    /// What was done, as one sentence; None when nothing was.
    pub fn done_sentence(&self) -> Option<String> {
        (!self.done.is_empty()).then(|| format!("{}.", self.done.join("; ")))
    }

    /// The model's note after a command that failed and won't run again
    /// by itself: what was done and that it can run again, then what's
    /// still missing. None when there's nothing to say.
    pub fn note(&self) -> Option<String> {
        let mut lines: Vec<String> = self
            .done_sentence()
            .map(|done| format!("{done} Run the command again."))
            .into_iter()
            .collect();
        lines.extend(self.missing.iter().cloned());
        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    fn add(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::There => {}
            Outcome::Made(done) => self.done.push(done),
            Outcome::Missing(why) => self.missing.push(why),
        }
    }
}

/// What making one thing available came to.
enum Outcome {
    /// It was there.
    There,
    /// It's there now: what was done.
    Made(String),
    /// It isn't: why, and what can be done.
    Missing(String),
}

/// How far a program that isn't on the PATH a command gets is looked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// To a cloud host's toolchains alone, which costs nothing where
    /// there's nothing to install: before each of the agent's commands
    /// ([`after`] looks further, once one failed).
    Host,
    /// To the login shell's PATH as well, read again (a login shell's
    /// start, up to a few seconds): before a command nothing runs again
    /// when it fails (Prepare's installs).
    Everywhere,
}

/// Before `command` runs: the programs it starts that an ecosystem brings
/// and that aren't there, made available as far as `look` goes. One that
/// can't be is the command's to fail on, which says it: the command may
/// make it itself (`npm i -g pnpm && pnpm i`).
pub async fn before(cx: &Context, command: &str, look: Look) -> Ready {
    let mut ready = Ready::default();
    for program in programs(command) {
        if ecosystems::of_program(&program).is_none() {
            continue;
        }
        if let Outcome::Made(done) = provide(cx, &program, look).await {
            ready.done.push(done);
        }
    }
    ready
}

/// [`before`] a command a harness's own shell is about to run (Claude
/// Code's, through its hook), as far as [`Look::Host`] goes. Nothing in
/// Plan mode: the harness runs its hook before it refuses a command, as it
/// refuses most there, and a version installed for a command that never
/// runs is minutes waited for nothing.
pub async fn before_harness_command(cx: &Context, command: &str) -> Ready {
    if cx.plan_mode {
        return Ready::default();
    }
    before(cx, command, Look::Host).await
}

/// After `command` failed with `exit_code` and `output`: what it didn't
/// find made available. A program an ecosystem brings is provided; a
/// package that isn't installed (`vite: not found`, `Cannot find module
/// 'react'`) has the project's dependency installs run; another program
/// is looked for on the login shell's PATH again, and said to be missing
/// only when the command names it: one its script ran (`vitest` for `npm
/// test`) may be a package's, which isn't the user's to install.
pub async fn after(cx: &Context, command: &str, exit_code: Option<i32>, output: &str) -> Ready {
    let mut ready = Ready::default();
    let root = Path::new(&cx.root);
    // The shared rustup, which installs nothing, names a version the host
    // lacks rather than a program the shell didn't find.
    if toolchains::on_host() && rustup_lacks_toolchain(output) {
        if let Some(rust) = ecosystems::toolchain_for("cargo", root) {
            ready.add(toolchain(cx, None, &rust).await);
        }
        return ready;
    }
    let missing = program_not_found(exit_code, output);
    if let Some(program) = missing
        .as_deref()
        .filter(|p| ecosystems::of_program(p).is_some())
    {
        ready.add(provide(cx, program, Look::Everywhere).await);
        return ready;
    }
    let asked = services_asked_for(cx, command).await;
    if !asked.is_empty() {
        for outcome in asked {
            ready.add(outcome);
        }
        return ready;
    }
    let services = services_needed(cx, output).await;
    if !services.is_empty() {
        for outcome in services {
            ready.add(outcome);
        }
        return ready;
    }
    if environment_failure(exit_code, output, std::slice::from_ref(&cx.root)).is_none() {
        return ready;
    }
    let installed = install_dependencies(cx, command).await;
    let none = installed.is_empty();
    for outcome in installed {
        ready.add(outcome);
    }
    if let Some(program) = missing.filter(|_| none) {
        match provide(cx, &program, Look::Everywhere).await {
            Outcome::Missing(_) if !programs(command).contains(&program) => {}
            outcome => ready.add(outcome),
        }
    }
    ready
}

/// The programs `command` starts, by name, each once: `cd web && yarn dev`
/// starts `cd` and `yarn`, past the variables set for one (`NODE_ENV=test
/// npm test`) and the wrappers that run the rest (`timeout 60 cargo
/// build`). One named by its path (`./run.sh`, `/usr/bin/node`) isn't
/// looked for on the PATH: it's left out.
pub fn programs(command: &str) -> Vec<String> {
    let parsed = permissions::parse_command(command);
    let mut names: Vec<String> = Vec::new();
    for segment in &parsed.segments {
        let set = segment.iter().take_while(|word| assignment(word)).count();
        let Some(first) = permissions::unwrap_wrappers(&segment[set..]).first() else {
            continue;
        };
        if first.contains(['/', '\\']) {
            continue;
        }
        let name = permissions::program_name(first);
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Whether `word` is a shell's `NAME=value`, which sets a variable for the
/// program after it.
fn assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Whether running `command` again repeats only its one program: a single
/// program call, nothing piped, written to a file or run in a subshell.
/// Such a command that failed for want of its program did nothing yet.
pub fn runs_one_program(command: &str) -> bool {
    let parsed = permissions::parse_command(command);
    parsed.segments.len() == 1 && !parsed.opaque && !parsed.writes_via_redirect
}

/// `program` made available for a command run in `cx.root`: on a cloud
/// host, the toolchain it comes with at the version the project runs, and
/// Yarn's or pnpm's through Corepack; else, or when that didn't bring it,
/// the login shell's PATH read again, where `look` goes that far.
async fn provide(cx: &Context, program: &str, look: Look) -> Outcome {
    let root = Path::new(&cx.root);
    if toolchains::found(root, program) {
        return Outcome::There;
    }
    let mut refused = None;
    if let Some(want) = ecosystems::toolchain_for(program, root).filter(|_| toolchains::on_host()) {
        let installed = match toolchain(cx, Some(program), &want).await {
            Outcome::Missing(why) => {
                refused = Some(why);
                None
            }
            Outcome::Made(done) => Some(done),
            Outcome::There => None,
        };
        let shim = refused.is_none() && toolchains::COREPACK_PROGRAMS.contains(&program);
        let shimmed = shim
            && toolchains::add_shim(program).unwrap_or_else(|why| {
                ::log::warn!("environment: {program} through Corepack: {why}");
                false
            });
        if toolchains::found(root, program) {
            let corepack = format!("`{program}` runs through Corepack, at the version the project's packageManager names");
            return Outcome::Made(match (installed, shimmed) {
                (Some(installed), true) => format!("{installed}; {corepack}"),
                (Some(installed), false) => installed,
                (None, true) => corepack,
                (None, false) => format!("`{program}` is installed for this project now"),
            });
        }
        if refused.is_none() {
            refused = Some(format!(
                "{} is installed for this project, and it has no `{program}`.",
                want.describe()
            ));
        }
    }
    if look == Look::Everywhere {
        read_login_path().await;
        if toolchains::found(root, program) {
            return Outcome::Made(format!(
                "`{program}` is installed: it wasn't on the PATH the command got, which has just been read again from the login shell"
            ));
        }
    }
    Outcome::Missing(refused.unwrap_or_else(|| nobody_installs(cx, program)))
}

/// Reads the login shell's PATH again, for the commands from now on: a
/// program installed since the app started is found (a server's setup
/// script runs after its daemon did). Tests only count it, on their
/// thread: that PATH is the whole process's, which other tests' commands
/// get, and a login shell can take seconds to start.
async fn read_login_path() {
    #[cfg(test)]
    LOGIN_PATH_READS.with(|reads| reads.set(reads.get() + 1));
    #[cfg(not(any(test, feature = "testing")))]
    let _ = crate::rt::spawn_blocking(crate::process::refresh_login_path).await;
}

#[cfg(test)]
thread_local! {
    /// How many times this thread asked for the login shell's PATH to be
    /// read again.
    static LOGIN_PATH_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// `want` on this cloud host, installed as root where it isn't: what was
/// installed, or why it couldn't be and how an administrator can.
/// `program`: what the command didn't find, for the words; None when it
/// was the version itself (rustup's).
async fn toolchain(cx: &Context, program: Option<&str>, want: &Want) -> Outcome {
    if toolchains::installed(want) {
        return Outcome::There;
    }
    let what = program.map_or_else(|| want.describe(), |program| format!("`{program}`"));
    let admin = format!(
        "`sudo {ROOT_COMMAND} admin toolchain add {} {}`",
        want.kind.name(),
        want.version
    );
    if !toolchains::can_install() {
        return Outcome::Missing(format!(
            "{what} isn't installed on this server, and its administrators don't let chats install toolchains. Tell the user to ask one of them to run {admin}."
        ));
    }
    (cx.note)(Some(format!(
        "Installing {} for this project…",
        want.describe()
    )));
    let done = toolchains::ensure(std::slice::from_ref(want)).await;
    (cx.note)(None);
    match done.into_iter().next() {
        Some((_, Err(why))) => {
            let it = match program {
                Some(_) => want.describe(),
                None => "it".into(),
            };
            Outcome::Missing(format!(
                "{what} isn't installed on this server, and {it} couldn't be installed: {}\nTell the user: an administrator can install it with {admin}.",
                why.trim()
            ))
        }
        _ => Outcome::Made(format!(
            "{} was installed for this project",
            want.describe()
        )),
    }
}

/// What the model is told of a program nothing here installs.
fn nobody_installs(cx: &Context, program: &str) -> String {
    match cx.core.agents.on_server() {
        true => format!(
            "`{program}` isn't installed on this server. Tell the user: the host's setup script, in Settings → Cloud hosts, installs what the project needs (e.g. Rust with `curl https://sh.rustup.rs -sSf | sh -s -- -y`), then Run setup."
        ),
        false => format!(
            "`{program}` isn't installed on this computer, or isn't on the login shell's PATH. Tell the user, who can install it: Rovik doesn't install programs here."
        ),
    }
}

/// The project's dependency installs for `command`, where what they
/// install isn't there: those of the ecosystems whose programs it starts,
/// or every one's when it starts none of theirs (`make dev`). Each runs as
/// Prepare's do, and isn't run again for [`INSTALL_AGAIN_AFTER`]; one the
/// user's permissions would ask about first is the model's to run, which
/// it's told.
async fn install_dependencies(cx: &Context, command: &str) -> Vec<Outcome> {
    if cx.plan_mode {
        return Vec::new();
    }
    let mut started: Vec<&'static dyn Ecosystem> = Vec::new();
    for ecosystem in programs(command)
        .iter()
        .filter_map(|p| ecosystems::of_program(p))
    {
        if !started.iter().any(|e| e.name() == ecosystem.name()) {
            started.push(ecosystem);
        }
    }
    let of = match started.is_empty() {
        true => ecosystems::ALL.to_vec(),
        false => started,
    };
    let installs = ecosystems::installs_of(&of, Path::new(&cx.root));
    let mut outcomes = Vec::new();
    for (index, install) in installs.iter().enumerate() {
        let place = match install.folder.as_str() {
            "" => String::new(),
            folder => format!(" in {folder}"),
        };
        if !may_install(cx, &install.command) {
            outcomes.push(Outcome::Missing(format!(
                "The project's dependencies aren't installed{place}: run `{}`{place}, then the command again.",
                install.command
            )));
            continue;
        }
        if !first_lately(cx, install) {
            continue;
        }
        (cx.note)(Some(format!(
            "Installing the dependencies: {}{place}…",
            install.command
        )));
        let timeout = super::verify::check_timeout_ms(&cx.chat);
        let result = super::prepare::run_install(
            &cx.chat, &cx.root, install, index, "install", &cx.stop, timeout,
        )
        .await;
        (cx.note)(None);
        let saved = result
            .saved_output
            .as_deref()
            .map(|saved| format!(" (its output: {saved})"))
            .unwrap_or_default();
        outcomes.push(match result.status {
            "passed" => Outcome::Made(format!(
                "The project's dependencies weren't installed{place}: `{}` ran{saved}",
                install.command
            )),
            "skipped" => Outcome::There,
            _ => {
                let first = result
                    .output
                    .as_deref()
                    .and_then(|o| o.lines().map(str::trim).find(|l| !l.is_empty()))
                    .unwrap_or("it said nothing")
                    .to_string();
                Outcome::Missing(format!(
                    "The project's dependencies aren't installed{place}: `{}` failed{saved}: {first}",
                    install.command
                ))
            }
        });
    }
    outcomes
}

/// Whether Rovik may run `command`, an install, for one of `cx.chat`'s:
/// where the user's permissions run it without anyone asked first. An
/// Autopilot task's run, which nobody attends, goes by its Prepare setting
/// instead, as its installs do (D19).
fn may_install(cx: &Context, command: &str) -> bool {
    let config = cx.core.agents.config_for(&cx.chat);
    let permitted = matches!(
        permissions::check_command(command, &config.permissions),
        Verdict::Permitted { .. }
    );
    let unattended = cx.core.agents.autopilot_of(&cx.chat).is_some();
    permitted
        && match unattended {
            true => config.autopilot.prepare,
            false => !config.permissions.command_approval.required,
        }
}

/// The installs run lately, for which chat and folder: when.
static RAN: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);

/// Whether `install` hasn't run for `cx`'s chat lately; it counts as run
/// from now.
fn first_lately(cx: &Context, install: &Install) -> bool {
    let key = format!(
        "{}\0{}\0{}\0{}",
        cx.chat, cx.root, install.folder, install.command
    );
    let mut ran = RAN.lock().unwrap_or_else(|e| e.into_inner());
    let ran = ran.get_or_insert_with(HashMap::new);
    ran.retain(|_, at| at.elapsed() < INSTALL_AGAIN_AFTER);
    ran.insert(key, Instant::now()).is_none()
}

/// The project's services a failed command needed and didn't find (each
/// backing service of its compose files publishing a local port the
/// command was refused at, or named by a host it couldn't look up), each
/// file's started where this machine runs them, or the model told what to
/// do. Empty when the output names none: Compose isn't even asked then.
async fn services_needed(cx: &Context, output: &str) -> Vec<Outcome> {
    let refused = super::unreached::refused_ports(output);
    let unresolved = super::unreached::unresolved_hosts(output);
    if refused.is_empty() && unresolved.is_empty() {
        return Vec::new();
    }
    let Some(program) = services::compose::program() else {
        return Vec::new();
    };
    let root = PathBuf::from(&cx.root);
    let models = crate::rt::spawn_blocking(move || services::compose::read_all(&program, &root))
        .await
        .unwrap_or_default();
    let mut outcomes = Vec::new();
    for model in models {
        let needed = services::needed(&model, &refused, &unresolved);
        if !needed.is_empty() {
            outcomes.extend(start_services(cx, model, needed).await);
        }
    }
    outcomes
}

/// After a command that asked a container engine's Compose to start the
/// project's services, which a cloud host gives the agent's commands no
/// engine for: Rovik starts them itself, as for a command that needed one,
/// or says why it can't, and the command isn't run again. Nothing for
/// another command, or on the user's own computer, where the command
/// reached the user's engine or the sandbox kept it from it (the agent's
/// prompt says to ask the user there).
async fn services_asked_for(cx: &Context, command: &str) -> Vec<Outcome> {
    let Some(names) = compose_up(command) else {
        return Vec::new();
    };
    if services::host().is_none() {
        return Vec::new();
    }
    let Some(program) = services::compose::program() else {
        return Vec::new();
    };
    let root = PathBuf::from(&cx.root);
    let models = crate::rt::spawn_blocking(move || services::compose::read_all(&program, &root))
        .await
        .unwrap_or_default();
    let Some((model, needed)) = models.into_iter().find_map(|model| {
        let asked = services::asked(&model, &names);
        (!asked.is_empty()).then_some((model, asked))
    }) else {
        return Vec::new();
    };
    let outcomes = start_services(cx, model, needed).await;
    outcomes
        .into_iter()
        .map(|outcome| match outcome {
            // Said, not run again: the engine it asked for isn't there.
            Outcome::Made(done) => Outcome::Missing(format!("{done}. {}", services::NO_ENGINE)),
            other => other,
        })
        .collect()
}

/// The services `command` asks Compose to start (`docker compose up -d
/// db`, `docker-compose up`, `podman compose up`, `podman-compose up`):
/// those it names, none for all of them. None for another command, or one
/// that names a compose file or a project of its own before `up` (`-f`,
/// `-p`), which may not be the file Rovik reads.
fn compose_up(command: &str) -> Option<Vec<String>> {
    let parsed = permissions::parse_command(command);
    parsed.segments.iter().find_map(|segment| {
        let set = segment.iter().take_while(|word| assignment(word)).count();
        let (first, rest) = permissions::unwrap_wrappers(&segment[set..]).split_first()?;
        let program = permissions::program_name(first);
        let rest = match (program.as_str(), rest.first().map(String::as_str)) {
            ("docker" | "podman", Some("compose")) => &rest[1..],
            ("docker-compose" | "podman-compose", _) => rest,
            _ => return None,
        };
        let (up, args) = rest.split_first()?;
        if up != "up" {
            return None;
        }
        let mut names = Vec::new();
        let mut words = args.iter();
        while let Some(word) = words.next() {
            match word.as_str() {
                // `up`'s options that take a value as the next word.
                "--scale" | "-t" | "--timeout" | "--wait-timeout" | "--exit-code-from"
                | "--attach" | "--no-attach" | "--pull" => {
                    words.next();
                }
                option if option.starts_with('-') => {}
                name => names.push(name.to_string()),
            }
        }
        Some(names)
    })
}

/// `needed` of `model` started, where this machine runs the projects'
/// services and the run isn't in Plan mode; else what the model is told.
/// A start that failed isn't tried again for that chat for a while: it
/// would fail the same way after every command.
async fn start_services(cx: &Context, model: Arc<Model>, needed: Vec<Needed>) -> Vec<Outcome> {
    let host = services::host();
    let why = match &host {
        Some(services::Host::Runs(_)) if cx.plan_mode => NotStarted::PlanMode,
        Some(services::Host::Runs(runner)) => {
            let names: Vec<String> = needed.iter().map(|n| n.name.clone()).collect();
            let key = format!("{}\0{}\0{}", cx.chat, model.dir.display(), names.join(" "));
            if let Some(why) = failed_lately(&key) {
                return vec![Outcome::Missing(services::not_started_because(
                    &model, &needed, &why,
                ))];
            }
            (cx.note)(Some(format!(
                "Starting the project's {}: {}…",
                match names.len() {
                    1 => "service",
                    _ => "services",
                },
                names.join(", ")
            )));
            let started = runner.start(cx.chat.clone(), model.clone(), names).await;
            (cx.note)(None);
            let mut outcomes = vec![match started {
                Ok(said) => Outcome::Made(services::started(&model, &needed, &said)),
                Err(why) => {
                    remember_failure(key, &why);
                    Outcome::Missing(services::not_started_because(&model, &needed, &why))
                }
            }];
            // Started or not, a lookup by name won't find it: where to point
            // the command instead.
            outcomes.extend(services::by_name_note(&model, &needed).map(Outcome::Missing));
            return outcomes;
        }
        Some(services::Host::Cannot(why)) => NotStarted::Cannot(why),
        None => NotStarted::Here,
    };
    vec![Outcome::Missing(services::not_started_note(
        &model, &needed, why,
    ))]
}

/// The services' starts that failed lately, for which chat and services:
/// when, and why.
static FAILED: Mutex<Option<HashMap<String, (Instant, String)>>> = Mutex::new(None);

/// Why the start under `key` failed, when it failed lately.
fn failed_lately(key: &str) -> Option<String> {
    let mut failed = FAILED.lock().unwrap_or_else(|e| e.into_inner());
    let failed = failed.get_or_insert_with(HashMap::new);
    failed.retain(|_, (at, _)| at.elapsed() < INSTALL_AGAIN_AFTER);
    failed.get(key).map(|(at, why)| {
        format!(
            "it failed {} ago, and isn't tried again for a while: {why}",
            minutes(at.elapsed())
        )
    })
}

fn remember_failure(key: String, why: &str) {
    let mut failed = FAILED.lock().unwrap_or_else(|e| e.into_inner());
    failed
        .get_or_insert_with(HashMap::new)
        .insert(key, (Instant::now(), why.to_string()));
}

/// `elapsed` in words: "less than a minute", "3 minutes".
fn minutes(elapsed: Duration) -> String {
    match elapsed.as_secs() / 60 {
        0 => "less than a minute".to_string(),
        1 => "a minute".to_string(),
        n => format!("{n} minutes"),
    }
}

/// Whether `text` is rustup saying the toolchain a command asked for isn't
/// installed: `error: toolchain '1.82-x86_64-unknown-linux-gnu' is not
/// installed`.
fn rustup_lacks_toolchain(text: &str) -> bool {
    text.lines()
        .any(|line| line.contains("toolchain '") && line.contains("' is not installed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolchains::tests::put_node;

    #[test]
    fn a_commands_programs_are_those_its_parts_start() {
        assert_eq!(programs("cd web && yarn dev"), ["cd", "yarn"]);
        assert_eq!(
            programs("NODE_ENV=test npm run dev | tee log.txt"),
            ["npm", "tee"]
        );
        assert_eq!(programs("timeout 60 cargo build; cargo test"), ["cargo"]);
        assert_eq!(
            programs("./run.sh && /usr/bin/node x.js"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn only_a_lone_program_call_runs_again_by_itself() {
        assert!(runs_one_program("npm run dev"));
        assert!(runs_one_program("cargo test --workspace"));
        for command in [
            "cd web && npm run dev",
            "npm test | tail -5",
            "npm test > out.txt",
            "echo $(node -v)",
            "rm -rf dist; npm run build",
        ] {
            assert!(!runs_one_program(command), "{command}");
        }
    }

    #[test]
    fn rustups_missing_version_is_told_from_other_failures() {
        assert!(rustup_lacks_toolchain(
            "error: toolchain '1.82-x86_64-unknown-linux-gnu' is not installed\n"
        ));
        assert!(!rustup_lacks_toolchain(
            "error[E0425]: cannot find value `x`"
        ));
    }

    /// A cloud host for a test: its shared folder, its root command (which
    /// gets each version asked for), and the user's folder of package
    /// managers. Tests that set one wait for each other.
    struct Host {
        dir: crate::testing::TempDir,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Host {
        /// A host whose root command `installer` makes for its folder.
        fn new(installer: impl FnOnce(&Path) -> Option<Arc<toolchains::Installer>>) -> Host {
            let lock = toolchains::test_lock();
            let dir = crate::testing::temp_dir();
            let root = dir.0.join("toolchains");
            std::fs::create_dir_all(root.join("node")).unwrap();
            toolchains::set(toolchains::Shared {
                installer: installer(&root),
                root,
                caches: None,
                bin: Some(dir.0.join("bin")),
            });
            Host { dir, _lock: lock }
        }

        fn root(&self) -> std::path::PathBuf {
            self.dir.0.join("toolchains")
        }
    }

    impl Drop for Host {
        fn drop(&mut self) {
            toolchains::unset();
        }
    }

    /// A project with `files` (path, content), and a context for its chat
    /// whose notes are kept.
    fn project(
        files: &[(&str, &str)],
    ) -> (
        crate::testing::TestCore,
        Context,
        Arc<Mutex<Vec<Option<String>>>>,
    ) {
        let test = crate::testing::TestCore::new();
        let root = test.dir.0.join("project");
        for (path, content) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        std::fs::create_dir_all(&root).unwrap();
        let notes = Arc::new(Mutex::new(Vec::new()));
        let kept = notes.clone();
        let cx = Context {
            core: test.core.clone(),
            chat: uuid::Uuid::new_v4().to_string(),
            root: root.to_string_lossy().into_owned(),
            stop: Arc::new(StopSignal::new()),
            note: Arc::new(move |note| kept.lock().unwrap().push(note)),
            plan_mode: false,
        };
        (test, cx, notes)
    }

    /// What a project's compose file says through Compose: a database and
    /// a cache with their ports, and the app built from its Dockerfile.
    #[cfg(unix)]
    const SHOP: &str = r#"{"name": "shop", "services": {
      "web": {"build": {"context": "/p"}, "ports": [{"target": 3000, "published": "3000", "protocol": "tcp"}]},
      "db": {"image": "postgres:16-alpine", "ports": [{"target": 5432, "published": "5432", "protocol": "tcp"}]},
      "cache": {"image": "redis:7-alpine", "ports": [{"target": 6379, "published": "6379", "protocol": "tcp"}]}
    }}"#;

    /// A Compose that prints `json` for any project, in `dir`.
    #[cfg(unix)]
    fn fake_compose(dir: &Path, json: &str) -> services::compose::Program {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-compose");
        std::fs::write(&path, format!("#!/bin/sh\ncat <<'JSON'\n{json}\nJSON\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        services::compose::Program::Standalone(path)
    }

    /// Each start a daemon was asked for: its chat, compose project and
    /// services.
    #[cfg(unix)]
    type Asked = Arc<Mutex<Vec<(String, String, Vec<String>)>>>;

    /// What a cloud host's daemon would be asked to start, answered with
    /// `answer`.
    #[cfg(unix)]
    struct Recording {
        asked: Asked,
        answer: Result<String, String>,
    }

    #[cfg(unix)]
    impl services::Runner for Recording {
        fn start(
            &self,
            chat: String,
            model: Arc<Model>,
            names: Vec<String>,
        ) -> futures_util::future::BoxFuture<'static, Result<String, String>> {
            self.asked
                .lock()
                .unwrap()
                .push((chat, model.name.clone(), names));
            let answer = self.answer.clone();
            Box::pin(async move { answer })
        }
    }

    /// A project with a compose file Compose reads as [`SHOP`], on a host
    /// whose daemon runs services, answering `answer`: what it's asked.
    #[cfg(unix)]
    fn shop_on_a_host(
        answer: Result<String, String>,
    ) -> (crate::testing::TestCore, Context, Asked) {
        let (test, cx, _notes) = project(&[("compose.yaml", "services: {}\n")]);
        services::compose::use_program(Some(fake_compose(&test.dir.0, SHOP)));
        let asked = Arc::new(Mutex::new(Vec::new()));
        services::set_host(services::Host::Runs(Arc::new(Recording {
            asked: asked.clone(),
            answer,
        })));
        (test, cx, asked)
    }

    /// A command refused at a port the project's database publishes, on a
    /// cloud host whose daemon runs the services: the database is started
    /// for the chat, the call says so meanwhile, and the command can run
    /// again.
    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_service_a_command_could_not_reach_is_started_where_the_host_runs_them() {
        let _lock = services::test_lock();
        let (_test, mut cx, asked) = shop_on_a_host(Ok("at localhost:5432".into()));
        let notes = Arc::new(Mutex::new(Vec::new()));
        let kept = notes.clone();
        cx.note = Arc::new(move |note| kept.lock().unwrap().push(note));
        let ready = after(
            &cx,
            "npm run dev",
            Some(1),
            "Error: connect ECONNREFUSED 127.0.0.1:5432",
        )
        .await;
        assert_eq!(
            ready.done,
            [
                "Rovik started the `db` (postgres:16-alpine) service of compose.yaml, which the \
              command needed (at localhost:5432)"
            ]
        );
        assert!(ready.missing.is_empty(), "{:?}", ready.missing);
        assert!(ready.note().unwrap().ends_with("Run the command again."));
        assert_eq!(
            *asked.lock().unwrap(),
            [(cx.chat.clone(), "shop".to_string(), vec!["db".to_string()])]
        );
        assert_eq!(
            *notes.lock().unwrap(),
            [
                Some("Starting the project's service: db…".to_string()),
                None
            ]
        );
    }

    #[test]
    fn a_compose_up_names_the_services_it_starts() {
        let named = |command: &str| compose_up(command);
        let some = |names: &[&str]| Some(names.iter().map(|n| n.to_string()).collect());
        assert_eq!(named("docker compose up -d db"), some(&["db"]));
        assert_eq!(named("docker-compose up"), some(&[]), "all of them");
        assert_eq!(
            named("podman compose up --wait --scale db=2 -t 30 cache"),
            some(&["cache"])
        );
        assert_eq!(
            named("podman-compose up -d db cache"),
            some(&["db", "cache"])
        );
        assert_eq!(named("cd api && docker compose up -d"), some(&[]));
        assert_eq!(
            named("env COMPOSE_PROFILES=x docker compose up db"),
            some(&["db"])
        );
        assert_eq!(
            named("COMPOSE_PROFILES=x docker compose up db"),
            some(&["db"])
        );
        for other in [
            "docker compose -f dev.yaml up",
            "docker compose ps",
            "docker run -d postgres",
            "docker compose",
            "npm run dev",
        ] {
            assert_eq!(named(other), None, "{other}");
        }
    }

    /// The agent's own `docker compose up`, on a cloud host whose commands
    /// have no container engine: Rovik starts what it names instead, says
    /// so, and doesn't run the command again, which would fail the same.
    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_compose_up_on_a_host_has_rovik_start_its_services() {
        let _lock = services::test_lock();
        let (_test, cx, asked) = shop_on_a_host(Ok("at localhost:5432".into()));
        let ready = after(
            &cx,
            "docker compose up -d db",
            Some(127),
            "sh: 1: docker: not found",
        )
        .await;
        assert!(ready.done.is_empty(), "not run again: {:?}", ready.done);
        let said = ready.missing.join("\n");
        assert!(
            said.starts_with(
                "Rovik started the `db` (postgres:16-alpine) service of compose.yaml, which the \
                 command needed (at localhost:5432). Your commands have no container engine"
            ),
            "{said}"
        );
        let all = after(
            &cx,
            "docker-compose up",
            Some(127),
            "docker-compose: not found",
        )
        .await;
        assert!(all.missing[0].contains("the `cache`"), "{:?}", all.missing);
        assert_eq!(
            *asked.lock().unwrap(),
            [
                (cx.chat.clone(), "shop".to_string(), vec!["db".to_string()]),
                (
                    cx.chat.clone(),
                    "shop".to_string(),
                    vec!["cache".to_string(), "db".to_string()]
                ),
            ]
        );
    }

    /// On the user's own computer the command reached their engine, or the
    /// sandbox kept it from it: Rovik starts nothing.
    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_compose_up_on_the_users_computer_is_left_to_them() {
        let _lock = services::test_lock();
        let (test, cx, _notes) = project(&[("compose.yaml", "services: {}\n")]);
        services::compose::use_program(Some(fake_compose(&test.dir.0, SHOP)));
        let ready = after(
            &cx,
            "docker compose up -d db",
            Some(1),
            "Cannot connect to the Docker daemon at unix:///var/run/docker.sock.",
        )
        .await;
        let said = format!("{:?}", ready);
        assert!(!said.contains("Rovik started"), "{said}");
    }

    /// A start that failed is said, and isn't tried again for that chat
    /// for a while: the next command is told why at once.
    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_failed_start_is_said_and_not_tried_again_for_a_while() {
        let _lock = services::test_lock();
        let (_test, cx, asked) = shop_on_a_host(Err("its image couldn't be pulled".into()));
        let refused = "redis.exceptions.ConnectionError: Error 111 connecting to localhost:6379. Connection refused.";
        let first = after(&cx, "pytest", Some(1), refused).await;
        assert_eq!(
            first.missing,
            ["The command needed the `cache` (redis:7-alpine) service of compose.yaml, which isn't \
              running, and Rovik couldn't start it: its image couldn't be pulled"]
        );
        let again = after(&cx, "pytest", Some(1), refused).await;
        assert!(
            again.missing[0].contains("it failed less than a minute ago, and isn't tried again"),
            "{:?}",
            again.missing
        );
        assert_eq!(asked.lock().unwrap().len(), 1, "asked once");
    }

    /// A service looked up by its name, as the app would inside Compose's
    /// network, is started all the same, and the command is told where its
    /// code reaches it.
    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_service_looked_up_by_its_name_is_started_and_pointed_at_localhost() {
        let _lock = services::test_lock();
        let (_test, cx, asked) = shop_on_a_host(Ok("at localhost:5432".into()));
        let ready = after(&cx, "npm start", Some(1), "Error: getaddrinfo ENOTFOUND db").await;
        assert_eq!(ready.done.len(), 1, "{ready:?}");
        assert!(
            ready.missing[0].contains("its code reaches `db` at localhost:5432 instead"),
            "{:?}",
            ready.missing
        );
        assert_eq!(asked.lock().unwrap()[0].2, ["db"]);
    }

    /// Plan mode starts nothing, on a host that would.
    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn plan_mode_starts_no_service() {
        let _lock = services::test_lock();
        let (_test, mut cx, asked) = shop_on_a_host(Ok("at localhost:5432".into()));
        cx.plan_mode = true;
        let ready = after(
            &cx,
            "npm test",
            Some(1),
            "connect ECONNREFUSED 127.0.0.1:5432",
        )
        .await;
        assert!(
            ready.missing[0].contains("Plan mode starts nothing"),
            "{ready:?}"
        );
        assert!(asked.lock().unwrap().is_empty());
    }

    /// On the user's own computer, and on a server that can't run them,
    /// the model is told what to do instead.
    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn where_no_daemon_runs_the_services_the_model_is_told_what_to_do() {
        let _lock = services::test_lock();
        let (test, cx, _notes) = project(&[("compose.yaml", "services: {}\n")]);
        services::compose::use_program(Some(fake_compose(&test.dir.0, SHOP)));
        let refused = "connect ECONNREFUSED 127.0.0.1:5432";
        let here = after(&cx, "npm test", Some(1), refused).await;
        assert!(
            here.missing[0].contains("ask the user to start it (`docker compose up -d db`)"),
            "{here:?}"
        );
        services::set_host(services::Host::Cannot("Podman isn't installed".into()));
        let server = after(&cx, "npm test", Some(1), refused).await;
        assert!(
            server.missing[0]
                .contains("this server can't run the project's services (Podman isn't installed)"),
            "{server:?}"
        );
        // The app's own port, or one no service publishes, is no service's.
        for other in [
            "connect ECONNREFUSED 127.0.0.1:3000",
            "connect ECONNREFUSED 127.0.0.1:9999",
        ] {
            assert_eq!(
                after(&cx, "npm test", Some(1), other).await,
                Ready::default(),
                "{other}"
            );
        }
    }

    /// The root command of a host that installs Node versions: each one
    /// asked for lands in the shared folder, and is counted.
    fn installing_node(
        root: &Path,
        asked: Arc<Mutex<Vec<String>>>,
    ) -> Option<Arc<toolchains::Installer>> {
        let root = root.to_path_buf();
        Some(Arc::new(move |want: Want| {
            asked.lock().unwrap().push(want.describe());
            put_node(&root, &format!("{}.0.0", want.version));
            Ok(())
        }))
    }

    /// Before `npm run dev` on a host whose Node the project's version
    /// isn't: it's installed through the root command, the call says so
    /// while it is, and the next command finds it.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_hosts_node_is_installed_before_a_command_that_starts_npm() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let _host = Host::new(|root| installing_node(root, asked.clone()));
        let (_test, cx, notes) = project(&[("package.json", "{}"), (".nvmrc", "20\n")]);
        let _lacking = toolchains::lacking(&["node", "npm", "npx"]);
        let ready = before(&cx, "npm run dev", Look::Host).await;
        assert_eq!(ready.done, ["Node 20 was installed for this project"]);
        assert!(ready.missing.is_empty(), "{ready:?}");
        assert_eq!(
            *notes.lock().unwrap(),
            [
                Some("Installing Node 20 for this project…".to_string()),
                None
            ]
        );
        assert!(toolchains::found(Path::new(&cx.root), "npm"));
        // Found now: nothing more to do, nothing asked of root again.
        assert_eq!(before(&cx, "npm test", Look::Host).await, Ready::default());
        assert_eq!(*asked.lock().unwrap(), ["Node 20"]);
    }

    /// A harness runs its hook before it refuses a command, as it refuses
    /// most in Plan mode: nothing is installed before one there. In Agent
    /// mode its command is made ready as Rovik's own are.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn nothing_is_installed_before_a_harness_command_in_plan_mode() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let _host = Host::new(|root| installing_node(root, asked.clone()));
        let (_test, mut cx, notes) = project(&[("package.json", "{}"), (".nvmrc", "20\n")]);
        let _lacking = toolchains::lacking(&["node", "npm", "npx"]);
        cx.plan_mode = true;
        let planning = before_harness_command(&cx, "npm test").await;
        assert_eq!(planning, Ready::default());
        assert!(asked.lock().unwrap().is_empty());
        assert!(notes.lock().unwrap().is_empty());
        cx.plan_mode = false;
        let working = before_harness_command(&cx, "npm test").await;
        assert_eq!(working.done, ["Node 20 was installed for this project"]);
        assert_eq!(*asked.lock().unwrap(), ["Node 20"]);
    }

    /// Yarn on a host's Node, which has none: through Corepack, by a shim
    /// in the user's folder, which the next command finds.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn yarn_runs_through_corepack_on_the_hosts_node() {
        let host = Host::new(|_| None);
        put_node(&host.root(), "22.3.0");
        let (_test, cx, _) = project(&[("package.json", r#"{ "packageManager": "yarn@4.5.0" }"#)]);
        let _lacking = toolchains::lacking(&["yarn"]);
        let ready = before(&cx, "cd . && yarn install", Look::Host).await;
        assert_eq!(
            ready.done,
            ["`yarn` runs through Corepack, at the version the project's packageManager names"]
        );
        assert!(host.dir.0.join("bin/yarn").is_file());
        assert!(toolchains::found(Path::new(&cx.root), "yarn"));
    }

    /// After a command whose shell didn't find `npm`: the project's Node
    /// is installed, and the model is told to run it again.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_command_that_didnt_find_its_program_can_run_again_once_its_installed() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let _host = Host::new(|root| installing_node(root, asked.clone()));
        let (_test, cx, _) =
            project(&[("web/package.json", r#"{ "engines": { "node": ">=22" } }"#)]);
        let _lacking = toolchains::lacking(&["node", "npm", "npx"]);
        let ready = after(
            &cx,
            "cd web && npm test",
            Some(127),
            "sh: 1: npm: not found\n",
        )
        .await;
        assert_eq!(
            ready.note().unwrap(),
            "Node 22 was installed for this project. Run the command again."
        );
        assert_eq!(*asked.lock().unwrap(), ["Node 22"]);
    }

    /// A version the root command refuses, or a host that installs nothing
    /// for chats: what's missing, and what an administrator runs.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_toolchain_the_host_doesnt_install_says_who_can() {
        let refusing: Arc<toolchains::Installer> =
            Arc::new(|_| Err("sudo: a password is required\n".into()));
        let host = Host::new(|_| Some(refusing));
        let (_test, cx, _) = project(&[("package.json", "{}"), (".nvmrc", "20\n")]);
        let _lacking = toolchains::lacking(&["node", "npm", "npx"]);
        let ready = after(&cx, "npm test", Some(127), "sh: 1: npm: not found").await;
        assert!(!ready.changed());
        assert_eq!(
            ready.missing,
            ["`npm` isn't installed on this server, and Node 20 couldn't be installed: sudo: a password is required\n\
              Tell the user: an administrator can install it with `sudo /opt/rovik/bin/rovik-agentd admin toolchain add node 20`."]
        );
        drop(host);
        let _host = Host::new(|_| None);
        let ready = after(&cx, "npm test", Some(127), "sh: 1: npm: not found").await;
        assert_eq!(
            ready.missing,
            ["`npm` isn't installed on this server, and its administrators don't let chats install toolchains. \
              Tell the user to ask one of them to run `sudo /opt/rovik/bin/rovik-agentd admin toolchain add node 20`."]
        );
    }

    /// The shared rustup, which installs nothing, names the version the
    /// project's toolchain file asks for: it's installed as root.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn rustups_missing_version_is_installed_on_a_host() {
        let Some(triple) = (match std::env::consts::ARCH {
            "x86_64" => Some("x86_64-unknown-linux-gnu"),
            "aarch64" => Some("aarch64-unknown-linux-gnu"),
            _ => None,
        }) else {
            return;
        };
        let _host = Host::new(|root| {
            std::fs::create_dir_all(root.join("rust/cargo/bin")).unwrap();
            let installed = root.join(format!("rust/rustup/toolchains/1.82-{triple}"));
            Some(Arc::new(move |want: Want| {
                assert_eq!(want.describe(), "Rust 1.82");
                std::fs::create_dir_all(&installed).unwrap();
                Ok(())
            }))
        });
        let (_test, cx, _) =
            project(&[("Cargo.toml", "[package]\n"), ("rust-toolchain", "1.82\n")]);
        let said = format!("error: toolchain '1.82-{triple}' is not installed\n");
        let ready = after(&cx, "cargo build", Some(1), &said).await;
        assert_eq!(ready.done, ["Rust 1.82 was installed for this project"]);
    }

    /// A program no ecosystem brings: the host's setup script's to install
    /// on a server, the user's on their computer.
    #[tokio::test]
    async fn a_program_nothing_installs_is_the_setup_scripts_or_the_users() {
        let (test, cx, _) = project(&[]);
        let said = "sh: 1: rovik-no-such-tool: not found";
        let here = after(&cx, "rovik-no-such-tool --help", Some(127), said).await;
        assert_eq!(
            here.missing,
            ["`rovik-no-such-tool` isn't installed on this computer, or isn't on the login shell's PATH. \
              Tell the user, who can install it: Rovik doesn't install programs here."]
        );
        test.core.agents.set_on_server();
        let there = after(&cx, "rovik-no-such-tool --help", Some(127), said).await;
        assert!(
            there.missing[0].starts_with("`rovik-no-such-tool` isn't installed on this server. Tell the user: the host's setup script"),
            "{there:?}"
        );
    }

    /// The login shell's PATH, which takes a login shell's start, is read
    /// again for a program that isn't there only where it's worth it: not
    /// before each of the agent's commands, but before one nothing runs
    /// again (Prepare's installs), and once a command failed for want of
    /// it.
    #[tokio::test]
    async fn the_login_shells_path_is_read_again_only_where_its_worth_it() {
        let (_test, cx, _) = project(&[("requirements.txt", "")]);
        let _lacking = toolchains::lacking(&["python3"]);
        let reads = || LOGIN_PATH_READS.with(|reads| reads.get());
        let first = reads();
        let ready = before(&cx, "python3 app.py", Look::Host).await;
        assert_eq!(ready, Ready::default());
        assert_eq!(reads(), first, "not before the agent's commands");
        before(&cx, "python3 app.py", Look::Everywhere).await;
        assert_eq!(reads(), first + 1, "before Prepare's installs");
        let said = "sh: 1: python3: not found";
        let failed = after(&cx, "python3 app.py", Some(127), said).await;
        assert_eq!(reads(), first + 2, "once it failed");
        assert!(
            failed.missing[0].starts_with("`python3` isn't installed on this computer"),
            "{failed:?}"
        );
    }

    /// A package that isn't installed: the project's install is the
    /// model's to run where the user's permissions would ask first, and
    /// nobody's in Plan mode.
    #[tokio::test]
    async fn a_missing_package_needs_the_projects_install() {
        let (_test, mut cx, _) = project(&[("package.json", "{}"), ("package-lock.json", "{}")]);
        let said = "sh: 1: vitest: not found";
        let asked = after(&cx, "npm test", Some(127), said).await;
        assert_eq!(
            asked.missing,
            ["The project's dependencies aren't installed: run `npm ci --no-audit --no-fund`, then the command again."]
        );
        cx.plan_mode = true;
        let planning = after(&cx, "npm test", Some(127), said).await;
        assert_eq!(
            planning,
            Ready::default(),
            "nothing installs, and `vitest` may be a package's"
        );
        // Not a missing program or package: nothing to do.
        cx.plan_mode = false;
        assert_eq!(
            after(
                &cx,
                "npm test",
                Some(1),
                "AssertionError: expected 1 to be 2"
            )
            .await,
            Ready::default()
        );
    }

    /// The user's permissions run it without asking: the install runs as
    /// Prepare's do, once lately, and the dependencies are there after it.
    #[tokio::test]
    async fn a_missing_package_is_installed_where_the_permissions_let_it_run() {
        let has_npm = std::process::Command::new("npm")
            .arg("--version")
            .output()
            .is_ok();
        if !has_npm {
            eprintln!("npm isn't installed: skipped");
            return;
        }
        let (test, cx, _) = project(&[
            (
                "package.json",
                r#"{ "name": "t", "version": "1.0.0", "private": true, "devDependencies": { "vitest": "file:vendor/vitest" } }"#,
            ),
            (
                "package-lock.json",
                r#"{ "name": "t", "version": "1.0.0", "lockfileVersion": 3, "requires": true, "packages": { "": { "name": "t", "version": "1.0.0", "devDependencies": { "vitest": "file:vendor/vitest" } }, "node_modules/vitest": { "resolved": "vendor/vitest", "link": true }, "vendor/vitest": { "version": "0.0.1", "dev": true, "bin": { "vitest": "bin.js" } } } }"#,
            ),
            (
                "vendor/vitest/package.json",
                r#"{ "name": "vitest", "version": "0.0.1", "bin": { "vitest": "bin.js" } }"#,
            ),
            (
                "vendor/vitest/bin.js",
                "#!/usr/bin/env node\nconsole.log('ran');\n",
            ),
        ]);
        let mut config = test.core.agents.config();
        config.permissions.allow_any_command = true;
        test.core
            .agents
            .set_resolver(Arc::new(move |_| Some(config.clone())));
        let ready = after(&cx, "npm test", Some(127), "sh: 1: vitest: not found").await;
        assert_eq!(ready.done.len(), 1, "{ready:?}");
        assert!(
            ready.done[0].starts_with("The project's dependencies weren't installed: `npm ci --no-audit --no-fund` ran (its output: @checks/install-"),
            "{ready:?}"
        );
        assert!(Path::new(&cx.root)
            .join("node_modules/.bin/vitest")
            .exists());
        // Installed: nothing runs again.
        let again = after(&cx, "npm test", Some(127), "sh: 1: vitest: not found").await;
        assert!(!again.changed(), "{again:?}");
    }

    #[test]
    fn the_note_says_what_was_done_then_what_is_missing() {
        let ready = Ready {
            done: vec![
                "Node 20.11.1 was installed for this project".into(),
                "`yarn` runs through Corepack".into(),
            ],
            missing: vec!["`make` isn't installed on this server.".into()],
        };
        assert_eq!(
            ready.note().unwrap(),
            "Node 20.11.1 was installed for this project; `yarn` runs through Corepack. Run the command again.\n`make` isn't installed on this server."
        );
        assert!(ready.changed());
        assert_eq!(Ready::default().note(), None);
        assert!(!Ready::default().changed());
    }
}
