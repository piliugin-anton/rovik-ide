//! Rovik's own Podman for this user (D21): the configuration, storage and
//! API service of the containers their projects' services run in, apart
//! from any Podman the user runs by hand. It all lives in
//! `~/.rovik/containers`, inside the daemon's folder, which the agent's
//! commands can't see: the API's socket gives whoever reaches it this
//! user's containers, and a container can mount anything of the user's.
//!
//! - `containers.conf` and `storage.conf`, which Podman reads in place of
//!   the user's (`CONTAINERS_CONF`, `CONTAINERS_STORAGE_CONF`): images,
//!   containers and volumes in `storage`, Podman's own run state in `run`,
//!   and no cgroups (`cgroups = "disabled"`): the containers stay in the
//!   cgroup of the API service that starts them, the daemon's unit, whose
//!   memory, CPU and task caps cover them too. The unit delegates no
//!   cgroup to its user, and a container that made one of its own would
//!   fail to start (Podman refuses even an unlimited pids limit, which
//!   Compose sends, without one). A service's own limits (`mem_limit`) are
//!   left unapplied: the user's caps are what bounds it.
//! - The API service, `podman system service`, started when a service is
//!   first started, which Docker Compose talks to through `run/podman.sock`.
//!   The kernel picks it and its containers when memory runs out, as the
//!   agent's commands, not the daemon itself.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// How long the API service may take to listen.
const API_WITHIN: Duration = Duration::from_secs(20);

/// How long a `podman` call may take, other than Compose's.
const CALL_WITHIN: Duration = Duration::from_secs(60);

/// This user's Podman for their projects' services.
pub struct Podman {
    /// `~/.rovik/containers`.
    dir: PathBuf,
    /// The pinned Docker Compose.
    compose: PathBuf,
    /// The API service, once started.
    api: tokio::sync::Mutex<Option<tokio::process::Child>>,
}

impl Podman {
    /// The Podman whose folder is `dir`, its services started with the
    /// Compose at `compose`.
    pub fn new(dir: PathBuf, compose: PathBuf) -> Podman {
        Podman {
            dir,
            compose,
            api: tokio::sync::Mutex::new(None),
        }
    }

    /// Its folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The folders and configuration files, made where they're missing and
    /// written again where they changed.
    pub fn prepare(&self) -> Result<(), String> {
        for dir in [
            self.dir.clone(),
            self.dir.join("run"),
            self.dir.join("tmp"),
            self.dir.join("storage"),
        ] {
            make_private_dir(&dir)?;
        }
        write_if_changed(&self.dir.join("containers.conf"), &containers_conf())?;
        write_if_changed(&self.dir.join("storage.conf"), &storage_conf(&self.dir))
    }

    /// What Podman and Compose run with: their configuration, their run
    /// state and temporary files in the daemon's folder, the user's home
    /// (their registry logins, `~/.config/containers/auth.json` or Docker's)
    /// and the PATH, and nothing else of the daemon's environment.
    fn env(&self) -> Vec<(&'static str, OsString)> {
        let mut env = vec![
            ("CONTAINERS_CONF", self.dir.join("containers.conf").into()),
            (
                "CONTAINERS_STORAGE_CONF",
                self.dir.join("storage.conf").into(),
            ),
            ("XDG_RUNTIME_DIR", self.dir.join("run").into()),
            ("TMPDIR", self.dir.join("tmp").into()),
        ];
        for key in ["HOME", "PATH", "USER", "LOGNAME"] {
            if let Some(value) = rovik_core::process::child_var(key) {
                env.push((key, value));
            }
        }
        env
    }

    /// The API's socket.
    pub fn socket(&self) -> PathBuf {
        self.dir.join("run/podman.sock")
    }

    /// A command running `program` with Podman's environment, its own
    /// cleared.
    fn command(&self, program: &Path) -> tokio::process::Command {
        let mut cmd = rovik_core::process::command(program);
        cmd.env_clear().envs(self.env()).stdin(Stdio::null());
        // Picked like the agent's commands when memory runs out, never
        // after the daemon (-500).
        rovik_core::process::ordinary_oom_score(&mut cmd);
        let mut cmd = tokio::process::Command::from(cmd);
        cmd.kill_on_drop(true);
        cmd
    }

    /// The API service's socket, the service started when it doesn't run.
    pub async fn api(&self) -> Result<PathBuf, String> {
        let socket = self.socket();
        let mut api = self.api.lock().await;
        if let Some(child) = api.as_mut() {
            if matches!(child.try_wait(), Ok(None)) && answers(&socket) {
                return Ok(socket);
            }
            let _ = child.kill().await;
        }
        self.prepare()?;
        let _ = std::fs::remove_file(&socket);
        let log = std::fs::File::create(self.dir.join("run/service.log"))
            .map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let mut cmd = self.command(Path::new("podman"));
        cmd.args(["system", "service", "--time=0"])
            .arg(format!("unix://{}", socket.display()))
            .stdout(Stdio::null())
            .stderr(log);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Podman's service couldn't start: {e}"))?;
        let deadline = tokio::time::Instant::now() + API_WITHIN;
        loop {
            if answers(&socket) {
                break;
            }
            if let Ok(Some(status)) = child.try_wait() {
                return Err(format!(
                    "Podman's service ended ({status}): {}",
                    tail(&self.dir.join("run/service.log"))
                ));
            }
            if tokio::time::Instant::now() >= deadline {
                let _ = child.kill().await;
                return Err(format!(
                    "Podman's service didn't listen within {} s: {}",
                    API_WITHIN.as_secs(),
                    tail(&self.dir.join("run/service.log"))
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        *api = Some(child);
        Ok(socket)
    }

    /// `podman ARGS`: what it printed, or why it failed.
    pub async fn podman(&self, args: &[&str]) -> Result<String, String> {
        self.podman_within(args, CALL_WITHIN).await
    }

    /// `podman`, given `within` to end: an image's copy may take minutes.
    pub async fn podman_within(&self, args: &[&str], within: Duration) -> Result<String, String> {
        self.prepare()?;
        let mut cmd = self.command(Path::new("podman"));
        cmd.args(args);
        run(cmd, None, within, &format!("podman {}", args.join(" "))).await
    }

    /// Docker Compose with the project name `project`, read in `dir`, with
    /// `args`, against the API service, `input` on its standard input:
    /// what it printed, or why it failed, within `within`.
    pub async fn compose(
        &self,
        project: &str,
        dir: &Path,
        args: &[String],
        input: Option<String>,
        within: Duration,
    ) -> Result<String, String> {
        let socket = self.api().await?;
        let mut cmd = self.command(&self.compose);
        cmd.arg("--project-name")
            .arg(project)
            .arg("--project-directory")
            .arg(dir)
            .args(args)
            .current_dir(dir)
            .env("DOCKER_HOST", format!("unix://{}", socket.display()));
        run(cmd, input, within, "Compose").await
    }

    /// The main process of `service`'s container in the Compose project
    /// `project`, where it runs.
    pub async fn pid_of(&self, project: &str, service: &str) -> Option<u32> {
        let id = self.container_of(project, service, true).await?;
        let said = self
            .podman(&["inspect", "--format", "{{.State.Pid}}", &id])
            .await
            .ok()?;
        said.trim().parse::<u32>().ok().filter(|&pid| pid > 0)
    }

    /// The container of `service` in `project`: one that runs, or with
    /// `running` false any.
    async fn container_of(&self, project: &str, service: &str, running: bool) -> Option<String> {
        let project = format!("label=com.docker.compose.project={project}");
        let service = format!("label=com.docker.compose.service={service}");
        let mut args = vec![
            "ps", "--filter", &project, "--filter", &service, "--format", "{{.ID}}",
        ];
        if !running {
            args.insert(1, "--all");
        }
        let said = self.podman(&args).await.ok()?;
        said.lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(str::to_string)
    }

    /// The services of `project` whose containers run now.
    pub async fn running(&self, project: &str) -> Vec<String> {
        let filter = format!("label=com.docker.compose.project={project}");
        let format = "{{index .Labels \"com.docker.compose.service\"}}";
        match self
            .podman(&["ps", "--filter", &filter, "--format", format])
            .await
        {
            Ok(said) => said
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// The last `lines` lines `service`'s container in `project` wrote:
    /// why it stopped, when it did.
    pub async fn logs(&self, project: &str, service: &str, lines: usize) -> Option<String> {
        let id = self.container_of(project, service, false).await?;
        let tail = lines.to_string();
        let said = self.podman(&["logs", "--tail", &tail, &id]).await.ok()?;
        let said = said.trim();
        (!said.is_empty()).then(|| said.to_string())
    }

    /// `path`, a folder a service's container may have written, removed as
    /// its user namespace's root: what a container's users wrote is owned
    /// by this user's subordinate ids, which the daemon can't remove alone.
    pub async fn remove(&self, path: &Path) -> Result<(), String> {
        if std::fs::remove_dir_all(path).is_ok() || !path.exists() {
            return Ok(());
        }
        let path = path.to_string_lossy().into_owned();
        self.podman(&["unshare", "rm", "-rf", "--", &path])
            .await
            .map(drop)
    }

    /// Every container of this Podman stopped, and the API service ended:
    /// as the daemon stops, so a database ends cleanly rather than be
    /// killed with the unit.
    pub async fn stop_all(&self) {
        let mut api = self.api.lock().await;
        if api.is_none() {
            return;
        }
        if let Err(why) = self.podman(&["stop", "--all", "--time", "10"]).await {
            log::warn!("the projects' services didn't all stop: {why}");
        }
        if let Some(mut child) = api.take() {
            let _ = child.kill().await;
        }
    }
}

/// Podman's settings for Rovik's containers.
fn containers_conf() -> String {
    "\
# Rovik's own Podman for this user's project services (rovik-agentd, D21).
# Written again by the daemon: change nothing here.
[containers]
cgroups = \"disabled\"
log_driver = \"k8s-file\"

[engine]
cgroup_manager = \"cgroupfs\"
events_logger = \"file\"
"
    .to_string()
}

/// Where Podman keeps Rovik's images and containers: in `dir`.
fn storage_conf(dir: &Path) -> String {
    let quoted = |path: &Path| format!("{:?}", path.to_string_lossy());
    format!(
        "\
# Rovik's own Podman for this user's project services (rovik-agentd, D21).
# Written again by the daemon: change nothing here.
[storage]
driver = \"overlay\"
graphroot = {}
runroot = {}
",
        quoted(&dir.join("storage")),
        quoted(&dir.join("run/storage")),
    )
}

/// The folder `dir`, this user's alone, made where it's missing.
fn make_private_dir(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .and_then(|()| std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)))
        .map_err(|e| format!("{}: {e}", dir.display()))
}

/// `text` at `path`, written only when it isn't what's there.
fn write_if_changed(path: &Path, text: &str) -> Result<(), String> {
    if std::fs::read_to_string(path).ok().as_deref() == Some(text) {
        return Ok(());
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Whether something accepts connections on the Unix socket `socket`.
fn answers(socket: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket).is_ok()
}

/// `cmd` run with `input` on its standard input, within `within`: what it
/// printed, or why it failed, its last lines named for `what`.
async fn run(
    mut cmd: tokio::process::Command,
    input: Option<String>,
    within: Duration,
    what: &str,
) -> Result<String, String> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    if input.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{what} couldn't start: {e}"))?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin
            .write_all(input.as_bytes())
            .await
            .map_err(|e| format!("{what}: {e}"))?;
    }
    let out = match tokio::time::timeout(within, child.wait_with_output()).await {
        Ok(out) => out.map_err(|e| format!("{what}: {e}"))?,
        Err(_) => {
            return Err(format!(
                "{what} took more than {} minutes, and was stopped",
                within.as_secs() / 60
            ))
        }
    };
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        return Ok(said);
    }
    let err = String::from_utf8_lossy(&out.stderr);
    Err(last_lines(&format!("{said}\n{err}"), 6))
}

/// The last `count` lines of `text` that say something, on one line.
pub(super) fn last_lines(text: &str, count: usize) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    match lines.is_empty() {
        true => "it said nothing".to_string(),
        false => lines[lines.len().saturating_sub(count)..].join(" / "),
    }
}

/// The end of the log at `path`, for a message.
fn tail(path: &Path) -> String {
    last_lines(&std::fs::read_to_string(path).unwrap_or_default(), 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rovik_s_podman_keeps_everything_in_its_folder() {
        let conf = storage_conf(Path::new("/home/ann/.rovik/containers"));
        assert!(
            conf.contains("graphroot = \"/home/ann/.rovik/containers/storage\"\n"),
            "{conf}"
        );
        assert!(
            conf.contains("runroot = \"/home/ann/.rovik/containers/run/storage\"\n"),
            "{conf}"
        );
        // No cgroups of its own: the containers stay under the daemon's
        // unit, whose caps cover them.
        assert!(containers_conf().contains("[containers]\ncgroups = \"disabled\"\n"));
    }

    #[test]
    fn the_folders_are_this_users_alone_and_rewritten_only_when_changed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = rovik_core::testing::temp_dir();
        let podman = Podman::new(dir.0.join("containers"), PathBuf::from("/bin/true"));
        podman.prepare().unwrap();
        for sub in ["", "run", "tmp", "storage"] {
            let mode = std::fs::metadata(dir.0.join("containers").join(sub))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700, "{sub}");
        }
        let conf = dir.0.join("containers/containers.conf");
        let before = std::fs::metadata(&conf).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        podman.prepare().unwrap();
        assert_eq!(
            std::fs::metadata(&conf).unwrap().modified().unwrap(),
            before
        );
        // Its environment is its own: Podman's settings, not the user's.
        let env: Vec<&str> = podman.env().iter().map(|(k, _)| *k).collect();
        assert!(env.starts_with(&[
            "CONTAINERS_CONF",
            "CONTAINERS_STORAGE_CONF",
            "XDG_RUNTIME_DIR",
            "TMPDIR"
        ]));
    }

    #[test]
    fn a_failure_is_told_by_its_last_lines() {
        assert_eq!(last_lines("\n a \n\nb\nc\n", 2), "b / c");
        assert_eq!(last_lines("   \n", 3), "it said nothing");
    }
}
