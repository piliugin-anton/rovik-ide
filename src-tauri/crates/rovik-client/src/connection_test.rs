//! The connection test (H04, design §7): one step at a time, each with its
//! result, so the hosts page shows how far it got and what's wrong. The
//! name and TCP; the handshake and the host's key; logging in; the probe,
//! a script that says in one exec what the server offers (`probe`); and,
//! once the daemon is deployed, its health and version through the
//! tunnel. A failed step that the next ones need ends the test.
//!
//! The first connection stops at the host's key, before any credential
//! goes: the step carries the key and its fingerprint, for the user to
//! compare and confirm, and the test runs again with the key pinned.

pub mod probe;

use crate::http;
use crate::ssh::{Ssh, SshAuth, SshConnector, SshError, SshTarget};
use probe::{Probe, PROBE};
use rovik_protocol::PROTOCOL;
use serde::Serialize;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// How long a name lookup, a TCP connection and the daemon's health may
/// each take.
const STEP_WAIT: Duration = Duration::from_secs(15);

/// What's tested.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Check {
    /// The name, and a TCP connection to the SSH port.
    Address,
    /// The handshake, and the host's key against the one pinned.
    HostKey,
    LogIn,
    /// The probe's findings, from here to `Api`.
    System,
    Account,
    Disk,
    Git,
    Chrome,
    /// User namespaces and FUSE: extras, not needed.
    Isolation,
    /// systemd, or the fallback without it.
    Service,
    /// Rovik on the server: its version and administrators, or how a
    /// deploy would install it.
    Install,
    /// A provider's API, from the server: one step an address.
    Api,
    /// The user's daemon, through the tunnel.
    Daemon,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Passed,
    /// It works, with something missing or worth knowing.
    Warning,
    Failed,
    /// The host's key isn't pinned yet: the user is to compare and confirm
    /// it.
    Confirm,
    /// Not tested, and why.
    Skipped,
}

/// A step's result.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub check: Check,
    pub outcome: Outcome,
    /// What was found, in a sentence or two for the hosts page.
    pub message: String,
    /// The host's key, when it's to be confirmed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_key: Option<SeenKey>,
}

/// A host key as the host showed it.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SeenKey {
    /// As OpenSSH writes it: what `Host::host_key` pins.
    pub key: String,
    /// Its SHA-256 fingerprint, as OpenSSH shows it.
    pub fingerprint: String,
}

impl Step {
    pub fn new(check: Check, outcome: Outcome, message: String) -> Step {
        Step {
            check,
            outcome,
            message,
            host_key: None,
        }
    }

    /// Whether the steps after it can go on.
    pub fn lets_through(&self) -> bool {
        !matches!(self.outcome, Outcome::Failed | Outcome::Confirm)
    }
}

/// What's tested besides the connection.
#[derive(Clone, Debug, Default)]
pub struct TestOptions {
    /// The providers' API addresses, tried from the server.
    pub api_urls: Vec<String>,
    /// The daemon is deployed: its health is asked for.
    pub deployed: bool,
}

/// The steps so far, each told to `report` as it ends.
struct Steps<'a> {
    done: Vec<Step>,
    report: &'a (dyn Fn(&Step) + Send + Sync),
}

impl Steps<'_> {
    /// Adds `step`, and says whether the next ones can go on.
    fn push(&mut self, step: Step) -> bool {
        (self.report)(&step);
        let through = step.lets_through();
        self.done.push(step);
        through
    }
}

/// Tests `connector`'s host, telling `report` each step as it ends, and
/// returns them all. The connection it makes stays with `connector`.
pub async fn test_connection(
    connector: &SshConnector,
    options: &TestOptions,
    report: &(dyn Fn(&Step) + Send + Sync),
) -> Vec<Step> {
    let mut steps = Steps {
        done: Vec::new(),
        report,
    };
    let target = connector.target();
    if !steps.push(reach(&target.host, target.port).await) {
        return steps.done;
    }
    if let Err(e) = target.pinned_fingerprint() {
        steps.push(Step::new(
            Check::HostKey,
            Outcome::Failed,
            format!("{e}. Remove the host and add it again."),
        ));
        return steps.done;
    }
    let ssh = match connector.session().await {
        Ok(ssh) => {
            steps.push(key_matches(target));
            steps.push(logged_in(target));
            ssh
        }
        Err(e) => {
            for step in refused(target, e) {
                steps.push(step);
            }
            return steps.done;
        }
    };
    let probe = match run_probe(&ssh, &options.api_urls).await {
        Ok(probe) => probe,
        Err(step) => {
            steps.push(step);
            return steps.done;
        }
    };
    for step in probe.findings() {
        steps.push(step);
    }
    if options.deployed || probe.socket {
        steps.push(daemon(connector).await);
    } else {
        steps.push(Step::new(
            Check::Daemon,
            Outcome::Skipped,
            "Rovik isn't deployed for you here yet.".into(),
        ));
    }
    steps.done
}

/// The name, then a TCP connection to the SSH port at one of its
/// addresses.
async fn reach(host: &str, port: u16) -> Step {
    let step = |outcome, message: String| Step::new(Check::Address, outcome, message);
    let named = host.parse::<IpAddr>().is_err();
    let addresses: Vec<SocketAddr> = match host.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => {
            let found = tokio::time::timeout(STEP_WAIT, tokio::net::lookup_host((host, port)));
            match found.await {
                Ok(Ok(found)) => found.collect(),
                Ok(Err(e)) => return step(Outcome::Failed, format!("{host} wasn't found: {e}.")),
                Err(_) => {
                    return step(Outcome::Failed, format!("Looking up {host} took too long."))
                }
            }
        }
    };
    let mut why = format!("{host} has no address");
    for address in &addresses {
        let ip = address.ip();
        match tokio::time::timeout(STEP_WAIT, tokio::net::TcpStream::connect(address)).await {
            Ok(Ok(_)) if named => {
                return step(
                    Outcome::Passed,
                    format!("{host} is {ip}, and it answers on port {port}."),
                )
            }
            Ok(Ok(_)) => return step(Outcome::Passed, format!("{ip} answers on port {port}.")),
            Ok(Err(e)) => why = format!("{ip} didn't take a connection on port {port}: {e}"),
            Err(_) => why = format!("{ip} didn't answer on port {port} in time"),
        }
    }
    step(Outcome::Failed, format!("{why}."))
}

/// The host's key is the one pinned.
fn key_matches(target: &SshTarget) -> Step {
    let fingerprint = target.pinned_fingerprint().ok().flatten();
    Step::new(
        Check::HostKey,
        Outcome::Passed,
        format!(
            "The server's key is the one you confirmed: {}.",
            fingerprint.unwrap_or_default()
        ),
    )
}

fn logged_in(target: &SshTarget) -> Step {
    let how = match &target.auth {
        SshAuth::Key { path, .. } => format!("with the key {}", path.display()),
        SshAuth::Agent { .. } => "with a key from your SSH agent".into(),
        SshAuth::Password(_) => "with your password".into(),
        SshAuth::KeyboardInteractive(_) => "with your password, at the server's prompts".into(),
    };
    Step::new(
        Check::LogIn,
        Outcome::Passed,
        format!("Logged in as {} {how}.", target.user),
    )
}

/// The steps a failed connection ends with: the handshake or the key, or
/// logging in once the key was checked.
fn refused(target: &SshTarget, e: SshError) -> Vec<Step> {
    let key = |outcome, message: String| Step::new(Check::HostKey, outcome, message);
    match e {
        SshError::Unreachable(why) => vec![key(
            Outcome::Failed,
            format!("The SSH handshake with {} failed: {why}", target.host),
        )],
        SshError::UnknownHostKey {
            key: line,
            fingerprint,
        } => {
            let mut step = key(
                Outcome::Confirm,
                format!(
                    "This is the first connection to {}. Check that its key's fingerprint is \
                     {fingerprint}, as whoever runs the server sees it, then confirm it. \
                     Nothing has been sent to the server yet.",
                    target.host
                ),
            );
            step.host_key = Some(SeenKey {
                key: line,
                fingerprint,
            });
            vec![step]
        }
        e @ SshError::HostKeyChanged { .. } => vec![key(Outcome::Failed, e.to_string())],
        SshError::Install(why) => vec![
            key_matches(target),
            logged_in(target),
            Step::new(Check::Install, Outcome::Failed, why),
        ],
        e => vec![
            key_matches(target),
            Step::new(Check::LogIn, Outcome::Failed, e.to_string()),
        ],
    }
}

/// Runs the probe, and reads what it says.
async fn run_probe(ssh: &Ssh, urls: &[String]) -> Result<Probe, Step> {
    let failed = |why: String| Step::new(Check::System, Outcome::Failed, why);
    probe_server(ssh, urls).await.map_err(failed)
}

/// What the server offers, as the probe says (H04, and the deploy).
pub(crate) async fn probe_server(ssh: &Ssh, urls: &[String]) -> Result<Probe, String> {
    // Chrome may take 30 s to start, and each address 10 s.
    let wait = Duration::from_secs(60 + 15 * urls.len() as u64);
    let command = probe::command(urls);
    let run = ssh.exec_with_input(&command, PROBE.as_bytes());
    let output = match tokio::time::timeout(wait, run).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(format!("The probe couldn't run on the server: {e}")),
        Err(_) => return Err("The probe took too long on the server.".into()),
    };
    let probe = Probe::parse(&String::from_utf8_lossy(&output.stdout));
    if probe.os.is_empty() {
        let said = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "The server didn't run the probe, a POSIX shell script: {}",
            said.trim()
        ));
    }
    Ok(probe)
}

/// The daemon's health and version, through the tunnel.
async fn daemon(connector: &SshConnector) -> Step {
    let step = |outcome, message: String| Step::new(Check::Daemon, outcome, message);
    match tokio::time::timeout(STEP_WAIT, http::health(connector)).await {
        Ok(Ok(health)) if health.protocol < PROTOCOL => step(
            Outcome::Warning,
            format!(
                "Your daemon, rovik-agentd {}, answers, but it's older than this app: Rovik on \
                 the server has to be upgraded.",
                health.version
            ),
        ),
        Ok(Ok(health)) => step(
            Outcome::Passed,
            format!(
                "Your daemon, rovik-agentd {}, answers (protocol {}).",
                health.version, health.protocol
            ),
        ),
        Ok(Err(fail)) => step(
            Outcome::Failed,
            format!("Your daemon doesn't answer: {fail}"),
        ),
        Err(_) => step(Outcome::Failed, "Your daemon didn't answer in time.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_address_that_takes_no_connection_ends_the_test() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let open = reach("127.0.0.1", port).await;
        assert_eq!(open.outcome, Outcome::Passed);
        assert_eq!(open.message, format!("127.0.0.1 answers on port {port}."));
        let named = reach("localhost", port).await;
        assert_eq!(named.outcome, Outcome::Passed, "{}", named.message);
        assert!(
            named.message.starts_with("localhost is "),
            "{}",
            named.message
        );
        drop(listener);
        let closed = reach("127.0.0.1", port).await;
        assert_eq!(closed.outcome, Outcome::Failed);
        assert!(!closed.lets_through());
        let unknown = reach("no-such-host.invalid", 22).await;
        assert_eq!(unknown.outcome, Outcome::Failed);
        let lookup = [
            "no-such-host.invalid wasn't found",
            "Looking up no-such-host.invalid",
        ];
        assert!(
            lookup.iter().any(|m| unknown.message.starts_with(m)),
            "{}",
            unknown.message
        );
    }

    #[test]
    fn steps_go_to_the_page_as_json() {
        let mut step = Step::new(Check::HostKey, Outcome::Confirm, "Check it.".into());
        step.host_key = Some(SeenKey {
            key: "ssh-ed25519 AAAA".into(),
            fingerprint: "SHA256:abc".into(),
        });
        assert_eq!(
            serde_json::to_value(&step).unwrap(),
            serde_json::json!({
                "check": "host-key",
                "outcome": "confirm",
                "message": "Check it.",
                "hostKey": { "key": "ssh-ed25519 AAAA", "fingerprint": "SHA256:abc" },
            })
        );
        let plain = Step::new(Check::LogIn, Outcome::Passed, "In.".into());
        let json = serde_json::to_value(&plain).unwrap();
        assert_eq!(json["check"], "log-in");
        assert!(json.get("hostKey").is_none());
    }
}
