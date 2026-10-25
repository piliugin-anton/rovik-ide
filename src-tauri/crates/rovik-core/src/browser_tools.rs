//! Web tools for the agent, backed by an embedded headless Chrome (eoka).
//!
//! - One browser + one tab, launched lazily on the first web tool call and
//!   shut down after `IDLE_SHUTDOWN` without use, so Chrome only costs memory
//!   while the agent is actually browsing.
//! - Stealth browser (`StealthConfig::default`: evasions, patched binary,
//!   human-like input), plus a Cloudflare-challenge solver that runs before
//!   every read, so bot checks don't block the agent.
//! - Driven over a pipe on Linux and macOS (`--remote-debugging-pipe`), not
//!   a DevTools port. The port listens on 127.0.0.1 without a password, so
//!   on a server others share, any of them could drive this browser, which
//!   runs as the user (docs/design/cloud-agent-dev-plan.md, D18). Windows
//!   keeps the port. On Linux a Chrome that listens on a TCP port anyway,
//!   from a flag in a Chrome flags file, is closed before it's used.
//! - The browser is the Chrome for Testing Rovik pins (`pinned_browser`,
//!   D19), as a cloud host or the app installed it, else Chromium, else
//!   Chrome.
//! - On a team server it starts in the user's private network (D20), as
//!   their commands do: its localhost is theirs, where the dev servers the
//!   agent left running listen, and no other user's servers are there.
//! - `browse_url` / `web_search` / `browser_screenshot` are read-only:
//!   navigate, read, screenshot. A prompt-injected page must never be
//!   followed.
//! - Only http(s) URLs. `file://` would let the model read any local file,
//!   bypassing the project-root sandbox the file tools enforce.

use crate::agent_run::StopSignal;
use crate::overlay::confine::network::Network;
use eoka::{Browser, CommandHook, Page, Result as EokaResult, StealthConfig};
use log::info;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::time::sleep;

const IDLE_SHUTDOWN: Duration = Duration::from_secs(5 * 60);
const IDLE_CHECK_EVERY: Duration = Duration::from_secs(30);
/// Upper bound for any single tool call, including a cold Chrome start.
const OP_TIMEOUT: Duration = Duration::from_secs(60);
/// Modest viewport: screenshots cost vision tokens roughly by pixel count.
const VIEWPORT: (u32, u32) = (1280, 800);
const DEFAULT_MAX_CHARS: usize = 15_000;
const MAX_LINKS: usize = 40;

const SEARCH_URL: &str = "https://html.duckduckgo.com/html/";

/// Reported when the user pressed Stop while a browser tool was running.
const STOPPED: &str = "Stopped by the user.";

/// Resolves once `stopped` is set (never, if nobody can set it anymore).
async fn stopped_wait(stopped: &mut tokio::sync::watch::Receiver<bool>) {
    loop {
        if *stopped.borrow() {
            return;
        }
        if stopped.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/*
 * ---------------------------------------------------------------------------
 * Browser lifecycle
 * ---------------------------------------------------------------------------
 */

struct Live {
    browser: Browser,
    page: Page,
    /// The proxy it was started with (Settings → Network).
    proxy: Option<crate::net::BrowserProxy>,
}

#[derive(Default)]
struct Inner {
    live: Option<Live>,
    last_used: Option<Instant>,
}

/// Managed Tauri state. Cloning shares the same browser.
#[derive(Clone, Default)]
pub struct BrowserState(Arc<Mutex<Inner>>);

impl BrowserState {
    /// Runs `op` against the shared tab, launching Chrome first if needed.
    /// The lock is held for the whole call, so tool calls run one at a time
    /// on the single tab. With `Some(stop)`, Stop ends the wait at once
    /// (dropping the operation); with `None`, only the timeout bounds it.
    async fn with_page<T, F, Fut>(&self, op: F, stop: Option<&StopSignal>) -> Result<T, String>
    where
        F: FnOnce(Page) -> Fut,
        Fut: Future<Output = Result<T, String>>,
    {
        self.with_page_timeout(op, OP_TIMEOUT, stop).await
    }

    /// Same, with an explicit upper bound.
    async fn with_page_timeout<T, F, Fut>(
        &self,
        op: F,
        timeout: Duration,
        stop: Option<&StopSignal>,
    ) -> Result<T, String>
    where
        F: FnOnce(Page) -> Fut,
        Fut: Future<Output = Result<T, String>>,
    {
        let run = async {
            let mut inner = self.0.lock().await;

            // Started again on a proxy set since.
            let proxy = crate::net::browser_proxy().await;
            if inner.live.as_ref().is_some_and(|live| live.proxy != proxy) {
                if let Some(old) = inner.live.take() {
                    let _ = old.browser.close().await;
                }
            }
            if inner.live.is_none() {
                let network = crate::overlay::confine::network::current();
                inner.live = Some(launch(proxy, network).await?);
                self.spawn_idle_watcher();
            }
            inner.last_used = Some(Instant::now());

            let page = inner.live.as_ref().expect("launched above").page.clone();
            let result = op(page.clone()).await;

            // A failed navigation (DNS error, 404) leaves Chrome healthy; a
            // dead CDP connection doesn't. Probe, and drop a dead browser so
            // the next call starts a fresh one instead of failing forever.
            if result.is_err() {
                let alive = tokio::time::timeout(Duration::from_secs(3), page.url())
                    .await
                    .map(|r| r.is_ok())
                    .unwrap_or(false);
                if !alive {
                    if let Some(dead) = inner.live.take() {
                        let _ = dead.browser.close().await;
                    }
                }
            }

            inner.last_used = Some(Instant::now());
            result
        };

        let bounded = async {
            tokio::time::timeout(timeout, run)
                .await
                .map_err(|_| format!("Browser operation timed out after {}s", timeout.as_secs()))?
        };
        let Some(stop) = stop else {
            return bounded.await;
        };
        // Already stopped before the wait started.
        if stop.is_set() {
            return Err(STOPPED.into());
        }
        let mut stopped = stop.subscribe();
        tokio::select! {
            // Dropping `bounded` ends the wait at once: the mutex guard and
            // the Chrome operation go away with it, so the tab is free for
            // the next call.
            biased;
            result = bounded => result,
            _ = stopped_wait(&mut stopped) => Err(STOPPED.into()),
        }
    }

    fn spawn_idle_watcher(&self) {
        let state = self.clone();
        crate::rt::spawn(async move {
            loop {
                tokio::time::sleep(IDLE_CHECK_EVERY).await;

                let mut inner = state.0.lock().await;
                if inner.live.is_none() {
                    return; // closed elsewhere
                }
                let idle = inner.last_used.map(|t| t.elapsed()).unwrap_or_default();
                if idle >= IDLE_SHUTDOWN {
                    if let Some(live) = inner.live.take() {
                        drop(inner);
                        let _ = live.browser.close().await;
                    }
                    return;
                }
            }
        });
    }

    /// Closes Chrome if it's running. Called on app exit.
    pub async fn shutdown(&self) {
        let live = self.0.lock().await.live.take();
        if let Some(live) = live {
            let _ = live.browser.close().await;
        }
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
impl BrowserState {
    /// The running Chrome's process id, if one is running.
    async fn chrome_pid(&self) -> Option<u32> {
        self.0.lock().await.live.as_ref()?.browser.process_id()
    }
}

/// Chromium, which the tools start rather than Chrome where both are
/// installed.
const CHROMIUM: &[&str] = if cfg!(target_os = "linux") {
    &[
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/snap/bin/chromium",
    ]
} else if cfg!(target_os = "macos") {
    &["/Applications/Chromium.app/Contents/MacOS/Chromium"]
} else {
    &[]
};

/// The browser the tools start, if one is installed: the Chrome for
/// Testing Rovik pins (D19), as a cloud host or the app installed it, else
/// Chromium, else Chrome.
pub fn chrome_path() -> Option<PathBuf> {
    crate::pinned_browser::installed()
        .or_else(|| {
            CHROMIUM
                .iter()
                .map(PathBuf::from)
                .find(|path| path.is_file())
        })
        .or_else(|| eoka::stealth::find_chrome().ok())
}

/// What the tools say when there's no browser to start.
fn no_browser() -> String {
    use crate::pinned_browser::{downloading, NAME};
    if let Some((received, total)) = downloading() {
        let percent = received * 100 / total.max(1);
        return format!(
            "The browser for the web tools, {NAME}, is still being installed ({percent}%). \
             Try again when the app says it's ready."
        );
    }
    "No browser was found. Install Chromium or Google Chrome to use web tools. On a server \
     with Rovik installed for everyone, an administrator installs the browser Rovik pins \
     with: sudo /opt/rovik/bin/rovik-agentd admin browser"
        .into()
}

/// How the tools start the browser at `chrome`: headless, and on Unix over
/// a pipe, with no DevTools port (D18). Through `proxy` when Settings →
/// Network gives the browser one (eoka signs in to it), or directly with
/// `direct://`; without, Chrome's own settings, which follow the system's
/// proxy. In `network`, the user's private network on a team server (D20):
/// Chrome joins it as it starts, as the agent's commands do.
fn launch_config(
    chrome: &Path,
    proxy: Option<&crate::net::BrowserProxy>,
    network: Option<&Arc<Network>>,
) -> StealthConfig {
    let mut config = StealthConfig {
        headless: true,
        viewport_width: VIEWPORT.0,
        viewport_height: VIEWPORT.1,
        chrome_path: Some(chrome.to_string_lossy().into_owned()),
        pipe: cfg!(unix),
        proxy: proxy.map(|p| p.server.clone()),
        proxy_username: proxy.and_then(|p| p.username.clone()),
        proxy_password: proxy.and_then(|p| p.password.as_ref().map(|secret| secret.0.clone())),
        ..StealthConfig::default()
    };
    // eoka puts its throwaway profile, and the folder it gives Chrome as
    // TMPDIR, under the host's /tmp. Snap Chromium has a private /tmp, where
    // neither exists. Use folders inside the snap's own writable area
    // instead.
    if let Some(snap) = snap_dirs(chrome) {
        config.user_data_dir = Some(snap.profile.to_string_lossy().into_owned());
        config.runtime_dir = Some(snap.runtime.to_string_lossy().into_owned());
    }
    // Its localhost is then the user's, where the servers the agent started
    // listen; the server's own, every user's, isn't reached.
    if let Some(network) = network.cloned() {
        config.command_hook = Some(CommandHook::new(move |command| network.join(command)));
    }
    config
}

/// Starts the browser, through `proxy` and in `network` ([`launch_config`]).
async fn launch(
    proxy: Option<crate::net::BrowserProxy>,
    network: Option<Arc<Network>>,
) -> Result<Live, String> {
    let chrome = chrome_path().ok_or_else(no_browser)?;
    let config = launch_config(&chrome, proxy.as_ref(), network.as_ref());
    // The snap's profile is wiped on every launch so each session starts
    // clean.
    if let Some(profile) = &config.user_data_dir {
        let _ = std::fs::remove_dir_all(profile);
    }

    let browser = Browser::launch_with_config(config).await.map_err(|e| {
        let place = match network {
            Some(_) => " in this user's private network",
            None => "",
        };
        format!("Failed to start Chrome ({}){place}: {e}", chrome.display())
    })?;
    let page = browser
        .new_blank_page()
        .await
        .map_err(|e| format!("Failed to open a browser tab: {e}"))?;

    // Rovik gives Chrome no port, but a Chrome flags file (the distribution's
    // CHROMIUM_FLAGS, a user's chromium-flags.conf) can add one, which anyone
    // on the machine could use to drive this browser.
    #[cfg(target_os = "linux")]
    if let Some(pid) = browser.process_id() {
        match listening_ports(pid) {
            Ok(ports) if !ports.is_empty() => {
                let _ = browser.close().await;
                let ports: Vec<String> = ports.iter().map(u16::to_string).collect();
                return Err(format!(
                    "Chrome ({}) listens on TCP port {}, which anyone on this machine could use \
                     to drive it, so the browser tools won't use it. A Chrome flags file adds \
                     --remote-debugging-port: remove it there.",
                    chrome.display(),
                    ports.join(", ")
                ));
            }
            Ok(_) => {}
            Err(error) => log::warn!("Couldn't check Chrome's ports: {error}"),
        }
    }

    Ok(Live {
        browser,
        page,
        proxy,
    })
}

/// The TCP ports process `pid` and those descended from it listen on, from
/// /proc: in its own network, the user's private one when it joined it.
#[cfg(target_os = "linux")]
fn listening_ports(pid: u32) -> std::io::Result<Vec<u16>> {
    let sockets = crate::listening::sockets(&PathBuf::from(format!("/proc/{pid}/net")))?;
    let tree = crate::listening::process_tree(pid)?;
    let mut ports: Vec<u16> = crate::listening::holders(&sockets, tree)
        .into_iter()
        .map(|(_, port)| port)
        .collect();
    ports.sort_unstable();
    ports.dedup();
    Ok(ports)
}

/// The Chromium snap's folders for the browser: its profile, and the parent
/// of eoka's per-launch folder (Chrome's TMPDIR).
struct SnapDirs {
    profile: PathBuf,
    runtime: PathBuf,
}

/// `Some` when `chrome` is (or wraps) the Chromium snap.
fn snap_dirs(chrome: &Path) -> Option<SnapDirs> {
    if !cfg!(target_os = "linux") || !is_snap_chromium(chrome) {
        return None;
    }
    let common = PathBuf::from(std::env::var_os("HOME")?).join("snap/chromium/common");
    // Each of Rovik's processes its own: one wipes it at each launch.
    let profile = common.join(crate::os_lock::slotted("rovik-ide-browser"));
    // The folder's name before the app was renamed: moved over once, so the
    // browser keeps its cookies and logins.
    let legacy = common.join("tauri-ide-browser");
    if !profile.exists() && legacy.is_dir() {
        let _ = std::fs::rename(&legacy, &profile);
    }
    Some(SnapDirs {
        profile,
        runtime: common.join("rovik-ide-runtime"),
    })
}

fn is_snap_chromium(chrome: &Path) -> bool {
    // /snap/bin/chromium is a link to /usr/bin/snap: look before resolving.
    if chrome.starts_with("/snap/") {
        return true;
    }
    let resolved = std::fs::canonicalize(chrome).unwrap_or_else(|_| chrome.to_path_buf());
    if resolved.starts_with("/snap/") {
        return true;
    }
    // Ubuntu's /usr/bin/chromium-browser is a shell script that runs the snap.
    let head = std::fs::read(&resolved)
        .map(|bytes| bytes.into_iter().take(4096).collect::<Vec<u8>>())
        .unwrap_or_default();
    !head.starts_with(b"\x7fELF") && String::from_utf8_lossy(&head).contains("/snap/")
}

/*
 * ---------------------------------------------------------------------------
 * Helpers
 * ---------------------------------------------------------------------------
 */

fn validate_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    // Let the model write "example.com/docs" without a scheme.
    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let url = reqwest::Url::parse(&candidate).map_err(|e| format!("Invalid URL \"{raw}\": {e}"))?;
    match url.scheme() {
        "http" | "https" => Ok(url.to_string()),
        other => Err(format!("Only http(s) URLs are allowed, got \"{other}:\"")),
    }
}

/// Whether the browser may be pointed at `ip`: not a link-local address
/// (the cloud's metadata service, `169.254.169.254`), a private network's
/// (a router's or another machine's admin page), carrier-grade NAT's
/// (`100.100.100.200`, another cloud's metadata), IPv6's unique local
/// ones, or the unspecified address. Loopback is: the agent looks at its
/// own dev server there (`http://localhost:1420`).
fn reachable_ip(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            let shared = a == 100 && (64..128).contains(&b);
            !(v4.is_link_local() || v4.is_private() || v4.is_unspecified() || shared)
        }
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => reachable_ip(IpAddr::V4(v4)),
            None => !(v6.is_unicast_link_local() || v6.is_unique_local() || v6.is_unspecified()),
        },
    }
}

/// Refuses `url` (checked by `validate_url`) when its host is, or resolves
/// to, an address the browser isn't pointed at ([`reachable_ip`]): a page
/// the model opens could otherwise bring the cloud's credentials or a local
/// admin page into the chat. A name that doesn't resolve is left to the
/// browser to report.
async fn check_destination(url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("Invalid URL \"{url}\": {e}"))?;
    let port = parsed.port_or_known_default().unwrap_or(80);
    let host = parsed.host_str().unwrap_or_default();
    // An IPv6 address is written in brackets.
    let literal = host.trim_start_matches('[').trim_end_matches(']');
    let ips: Vec<std::net::IpAddr> = match literal.parse::<std::net::IpAddr>() {
        Ok(ip) => vec![ip],
        Err(_) if host.is_empty() => Vec::new(),
        Err(_) => match tokio::net::lookup_host((host, port)).await {
            Ok(addrs) => addrs.map(|a| a.ip()).collect(),
            Err(_) => Vec::new(),
        },
    };
    match ips.into_iter().find(|ip| !reachable_ip(*ip)) {
        Some(ip) => Err(format!(
            "{url} is at {ip}, a link-local or private network address, which the browser tools don't open. A local dev server is on localhost (127.0.0.1)."
        )),
        None => Ok(()),
    }
}

/// Navigates and waits until the page is usable: DOM parsed, then a short
/// best-effort wait for network quiet so client-rendered pages have content.
/// A Cloudflare challenge is solved first (best-effort: if it doesn't
/// clear, the read below still returns whatever is there).
async fn open(page: &Page, url: &str) -> Result<(), String> {
    page.goto(url)
        .await
        .map_err(|e| format!("Could not open {url}: {e}"))?;

    // Never fails the call: some pages have no challenge, some challenges
    // don't solve, and the readable text is still worth returning.
    let _ = settle_cloudflare(page).await;

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let state: String = page
            .evaluate_sync("document.readyState")
            .await
            .unwrap_or_default();
        if state == "interactive" || state == "complete" || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    // Never fails the call: some pages keep connections open forever.
    let _ = page.wait_for_network_idle(500, 5_000).await;
    Ok(())
}

/// Collapses runs of blank lines that innerText produces around hidden elements.
fn tidy_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0;
    for line in text.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_string()
}

/*
 * ---------------------------------------------------------------------------
 * Cloudflare bypass: detection + one-shot Turnstile solver
 * ---------------------------------------------------------------------------
 *
 * Read-only tools call `settle_cloudflare` before reading: if no challenge
 * is showing it returns at once, otherwise it clicks the widget's checkbox
 * once (human-like) and reports whether the challenge cleared. Best-effort
 * by design — a blocked page still returns whatever text it has.
 */

/// If a Cloudflare challenge is showing on `page`, run the solver once.
/// Ok(true) when no challenge was present or it cleared, Ok(false) when
/// it's still there. Err only on a transport-level failure.
async fn settle_cloudflare(page: &Page) -> EokaResult<bool> {
    if !is_cloudflare_challenge_active(page).await {
        return Ok(true);
    }
    solve_cloudflare_challenge(page).await?;
    Ok(!is_cloudflare_challenge_active(page).await)
}

/// True if a Cloudflare challenge appears to be active, using only signals
/// Cloudflare itself controls — never anything scraped from the host site.
async fn is_cloudflare_challenge_active(page: &Page) -> bool {
    // Cloudflare's own hardcoded interstitial title, same across every
    // site that uses their challenge product.
    if matches!(page.title().await, Ok(t) if t.trim() == "Just a moment...") {
        return true;
    }
    // Fallback: a live challenge iframe served from Cloudflare's own domain.
    if let Ok(frames) = page.frames().await {
        if frames
            .iter()
            .any(|f| f.url.contains("challenges.cloudflare.com"))
        {
            return true;
        }
    }
    false
}

async fn solve_cloudflare_challenge(page: &Page) -> EokaResult<()> {
    info!("CAPTCHA_DETECTED");

    if let Err(e) = wait_for_frame_load(
        page,
        "https://challenges.cloudflare.com/cdn-cgi/challenge-platform/",
        Duration::from_secs(15),
    )
    .await
    {
        info!("CAPTCHA_FRAME_NOT_FOUND: {e}");
        return Ok(());
    }

    info!("CAPTCHA_FRAME_LOADED");

    let header_el = match page
        .find("div.main-wrapper > div.main-content > h2[id]")
        .await
    {
        Ok(el) => el,
        Err(e) => {
            info!("CAPTCHA_HEADER_ELEMENT_NOT_FOUND: {e}");
            return Ok(());
        }
    };

    let rect = match header_el.bounding_box().await {
        Some(r) => r,
        None => {
            info!("CAPTCHA_HEADER_HAS_NO_BOUNDING_BOX");
            return Ok(());
        }
    };
    info!("GOT_CAPTCHA_HEADER_BOUNDING_BOX");

    // Turnstile's checkbox sits near the widget's left edge, vertically
    // centered — this is Cloudflare's own widget chrome, so the offset is
    // consistent regardless of which site embeds it.
    let x = 20.0 + random_f64(0.0, 2.0) + rect.x;
    let y = 111.0 + random_f64(0.0, 2.0) + rect.y + rect.height;
    info!("COMPUTED_CLICK_COORDS x={x} y={y}");

    page.wait(random_u64(2345, 4567)).await;
    page.human().move_and_click(x, y).await?;
    info!("CLICKED_CAPTCHA_CHECKBOX");

    page.wait_for_text(
        "Verifying you are human. This may take a few seconds.",
        3_000,
    )
    .await?;
    info!("CAPTCHA_IS_VERIFYING...");

    Ok(())
}

/// Wait for a frame matching `frame_url_part` to finish loading *and*
/// settle (no further DOM mutations), returning its frame id.
async fn wait_for_frame_load(
    page: &eoka::Page,
    frame_url_part: &str,
    timeout: Duration,
) -> Result<String, eoka::Error> {
    let deadline = Instant::now() + timeout;

    // 1. Find the frame and wait for readyState === "complete".
    let frame_id = loop {
        let mut found = None;
        for frame in page.frames().await? {
            if frame.url.contains(frame_url_part) {
                found = Some(frame.id.clone());
                break;
            }
        }

        if let Some(id) = found {
            match page
                .evaluate_in_frame_id::<String>(&id, "document.readyState")
                .await
            {
                Ok(ready) if ready == "complete" => break id,
                Ok(_) | Err(_) => {}
            }
        }

        if Instant::now() >= deadline {
            return Err(eoka::Error::Timeout(format!(
                "frame matching '{frame_url_part}' did not reach readyState=complete in time"
            )));
        }
        sleep(Duration::from_millis(50)).await;
    };

    // 2. Wait for the frame's DOM to stop mutating — two consecutive
    // snapshots of outerHTML that match, some quiet interval apart.
    wait_for_frame_stable(page, &frame_id, Duration::from_millis(300), deadline).await?;

    Ok(frame_id)
}

/// Poll a frame's outerHTML until it's unchanged across `quiet` interval,
/// or bail out at `deadline`.
async fn wait_for_frame_stable(
    page: &eoka::Page,
    frame_id: &str,
    quiet: Duration,
    deadline: Instant,
) -> Result<(), eoka::Error> {
    let mut last: Option<String> = None;

    loop {
        let snapshot: Result<String, _> = page
            .evaluate_in_frame_id(frame_id, "document.documentElement.outerHTML.length + ':' + document.documentElement.outerHTML")
            .await;

        if let Ok(snapshot) = snapshot {
            if last.as_deref() == Some(snapshot.as_str()) {
                return Ok(()); // unchanged since last check -> stable
            }
            last = Some(snapshot);
        }

        if Instant::now() >= deadline {
            return Err(eoka::Error::Timeout(format!(
                "frame {frame_id} did not stabilize in time"
            )));
        }
        sleep(quiet).await;
    }
}

fn random_f64(min: f64, max: f64) -> f64 {
    let mut rng = rand::rng();
    let value = rng.random_range(min..max);
    let sign = if rng.random_bool(0.5) { 1.0 } else { -1.0 };

    value * sign
}

fn random_u64(min: u64, max: u64) -> u64 {
    rand::rng().random_range(min..=max)
}

/*
 * ---------------------------------------------------------------------------
 * Commands
 * ---------------------------------------------------------------------------
 */

#[derive(Serialize, Deserialize)]
pub struct SearchHit {
    pub(crate) title: String,
    pub(crate) url: String,
    pub(crate) snippet: String,
}

/// DuckDuckGo's HTML endpoint: server-rendered, no JavaScript needed.
pub async fn web_search(
    state: &BrowserState,
    query: String,
    max_results: Option<usize>,
) -> Result<Vec<SearchHit>, String> {
    web_search_inner(state, query, max_results, None).await
}

/// `web_search` for an agent run: Stop ends the wait at once.
pub(crate) async fn web_search_inner(
    state: &BrowserState,
    query: String,
    max_results: Option<usize>,
    stop: Option<&StopSignal>,
) -> Result<Vec<SearchHit>, String> {
    let query = query.trim().to_string();
    if query.is_empty() {
        return Err("Search query is empty.".into());
    }
    let limit = max_results.unwrap_or(8).clamp(1, 20);
    let url = reqwest::Url::parse_with_params(SEARCH_URL, &[("q", query.as_str())])
        .map_err(|e| e.to_string())?
        .to_string();

    state
        .with_page(|page| async move {
            open(&page, &url).await?;

            // Result links are DDG redirects (/l/?uddg=<target>); unwrap them.
            let js = format!(
                r#"(() => {{
                    const unwrap = (href) => {{
                        try {{
                            const u = new URL(href, location.href);
                            return u.searchParams.get('uddg') || u.href;
                        }} catch {{ return href; }}
                    }};
                    return [...document.querySelectorAll('.result:not(.result--ad)')]
                        .map((r) => {{
                            const a = r.querySelector('.result__a');
                            return a ? {{
                                title: (a.innerText || '').trim(),
                                url: unwrap(a.getAttribute('href') || ''),
                                snippet: (r.querySelector('.result__snippet')?.innerText || '').trim(),
                            }} : null;
                        }})
                        .filter((x) => x && x.url.startsWith('http'))
                        .slice(0, {limit});
                }})()"#
            );
            let hits: Vec<SearchHit> = page
                .evaluate_sync(&js)
                .await
                .map_err(|e| format!("Could not read search results: {e}"))?;

            if hits.is_empty() {
                let body: String = page
                    .evaluate_sync("(document.body?.innerText || '').slice(0, 2000)")
                    .await
                    .unwrap_or_default();
                let lower = body.to_lowercase();
                if lower.contains("anomaly") || lower.contains("captcha") || lower.contains("robot") {
                    return Err(
                        "The search engine asked for a bot check. Try again later, or open a known URL with browse_url."
                            .into(),
                    );
                }
            }
            Ok(hits)
        },
            stop,
        )
        .await
}

#[derive(Serialize, Deserialize)]
pub struct PageLink {
    pub(crate) text: String,
    pub(crate) url: String,
}

#[derive(Serialize)]
pub struct BrowsedPage {
    pub(crate) url: String,
    pub(crate) title: String,
    pub(crate) text: String,
    pub(crate) truncated: bool,
    pub(crate) links: Vec<PageLink>,
}

#[derive(Deserialize)]
struct RawPage {
    url: String,
    title: String,
    text: String,
    links: Vec<PageLink>,
}

/// Opens a URL and returns its readable text plus the main links on it.
pub async fn browse_url(
    state: &BrowserState,
    url: String,
    max_chars: Option<usize>,
) -> Result<BrowsedPage, String> {
    browse_url_inner(state, url, max_chars, None).await
}

/// `browse_url` for an agent run: Stop ends the wait at once.
pub(crate) async fn browse_url_inner(
    state: &BrowserState,
    url: String,
    max_chars: Option<usize>,
    stop: Option<&StopSignal>,
) -> Result<BrowsedPage, String> {
    let url = validate_url(&url)?;
    check_destination(&url).await?;
    let max_chars = max_chars.unwrap_or(DEFAULT_MAX_CHARS).clamp(1_000, 60_000);

    state
        .with_page(
            |page| async move {
                open(&page, &url).await?;
                readable(&page, max_chars).await
            },
            stop,
        )
        .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserScreenshot {
    pub(crate) url: String,
    pub(crate) title: String,
    /// Stored in the chat database's attachments; shown by URL, and only
    /// base64-encoded when a request to a vision model is built.
    pub(crate) attachment_id: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// Screenshots the viewport, opening `url` first when given (otherwise the
/// page currently open in the agent's tab).
pub async fn browser_screenshot(
    core: &crate::core::Core,
    url: Option<String>,
) -> Result<BrowserScreenshot, String> {
    browser_screenshot_inner(&core.browser, &core.db, url, None).await
}

/// `browser_screenshot` for an agent run: Stop ends the wait at once.
pub(crate) async fn browser_screenshot_inner(
    state: &BrowserState,
    db: &crate::chat_db::DbState,
    url: Option<String>,
    stop: Option<&StopSignal>,
) -> Result<BrowserScreenshot, String> {
    let url = url
        .filter(|u| !u.trim().is_empty())
        .map(|u| validate_url(&u))
        .transpose()?;
    if let Some(url) = &url {
        check_destination(url).await?;
    }

    let (current, title, bytes) = state
        .with_page(
            |page| async move {
                if let Some(url) = &url {
                    open(&page, url).await?;
                }

                let current = page.url().await.unwrap_or_default();
                if current.is_empty() || current == "about:blank" {
                    return Err("No page is open yet. Pass a url to take a screenshot of.".into());
                }

                let bytes = page
                    .screenshot_jpeg(70)
                    .await
                    .map_err(|e| format!("Screenshot failed: {e}"))?;
                let title: String = page.title().await.unwrap_or_default();
                Ok((current, title, bytes))
            },
            stop,
        )
        .await?;

    let attachment_id = crate::chat_db::attachments::store(db, bytes, "image/jpeg").await?;
    Ok(BrowserScreenshot {
        url: current,
        title,
        attachment_id,
        width: VIEWPORT.0,
        height: VIEWPORT.1,
    })
}

/// The readable text of whatever the tab shows now.
async fn readable(page: &Page, max_chars: usize) -> Result<BrowsedPage, String> {
    // Prefer the main content region so nav/footer chrome doesn't
    // eat the character budget; fall back to the whole body.
    let js = format!(
        r#"(() => {{
            const root =
                document.querySelector('main, article, [role="main"]') || document.body;
            const seen = new Set();
            const links = [];
            for (const a of (root || document).querySelectorAll('a[href]')) {{
                const text = (a.innerText || a.getAttribute('aria-label') || '').trim().replace(/\s+/g, ' ');
                if (!text || !a.href.startsWith('http') || seen.has(a.href)) continue;
                seen.add(a.href);
                links.push({{ text: text.slice(0, 120), url: a.href }});
                if (links.length >= {MAX_LINKS}) break;
            }}
            return {{
                url: location.href,
                title: document.title || '',
                text: root ? root.innerText || '' : '',
                links,
            }};
        }})()"#
    );
    let raw: RawPage = page
        .evaluate_sync(&js)
        .await
        .map_err(|e| format!("Could not read the page: {e}"))?;

    let tidy = tidy_text(&raw.text);
    let (text, truncated) = crate::text::first_chars(&tidy, max_chars);
    Ok(BrowsedPage {
        url: raw.url,
        title: raw.title,
        text: text.to_string(),
        truncated,
        links: raw.links,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The browser tools open a local dev server and the web, not the
    /// cloud's metadata service or a private network's pages.
    #[tokio::test]
    async fn the_browser_isnt_pointed_at_link_local_or_private_addresses() {
        for refused in [
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.1/",
            "http://192.168.1.1/admin",
            "http://172.16.5.4:8080/",
            "http://100.100.100.200/latest/meta-data/",
            "http://[fe80::1]/",
            "http://[fd00:ec2::254]/",
            "http://[::ffff:169.254.169.254]/",
            "http://0.0.0.0:3000/",
        ] {
            let url = validate_url(refused).unwrap();
            let err = check_destination(&url).await.unwrap_err();
            assert!(err.contains("private network address"), "{refused}: {err}");
        }
        for allowed in [
            "http://127.0.0.1:3000/",
            "http://localhost:1420/",
            "http://[::1]:8080/",
            "https://93.184.215.14/",
        ] {
            let url = validate_url(allowed).unwrap();
            assert_eq!(check_destination(&url).await, Ok(()), "{allowed}");
        }
    }

    /// Stop before a browser call starts: it ends at once without launching
    /// Chrome (no browser needed for this test).
    #[tokio::test]
    async fn stop_before_the_start_runs_nothing() {
        let stop = Arc::new(StopSignal::new());
        stop.set();
        let state = BrowserState::default();
        let err = state
            .with_page(|_| async { Ok::<(), String>(()) }, Some(&stop))
            .await
            .unwrap_err();
        assert_eq!(err, "Stopped by the user.");
    }

    /// Settings → Network's proxy for the browser, which eoka signs in to;
    /// no proxy, `direct://`; none, Chrome's own settings.
    #[test]
    fn chrome_is_given_the_proxy_it_was_set() {
        let chrome = Path::new("/opt/rovik/browser/155.0.8059.39/chrome");
        let proxy = crate::net::BrowserProxy {
            server: "http://proxy.example.com:3128".into(),
            username: Some("alice".into()),
            password: Some(rovik_protocol::Secret("pw".into())),
        };
        let config = launch_config(chrome, Some(&proxy), None);
        assert_eq!(
            config.proxy.as_deref(),
            Some("http://proxy.example.com:3128")
        );
        assert_eq!(config.proxy_username.as_deref(), Some("alice"));
        assert_eq!(config.proxy_password.as_deref(), Some("pw"));
        let config = launch_config(chrome, Some(&crate::net::BrowserProxy::direct()), None);
        assert_eq!(
            (
                config.proxy.as_deref(),
                config.proxy_username,
                config.proxy_password
            ),
            (Some("direct://"), None, None)
        );
        let config = launch_config(chrome, None, None);
        assert_eq!(
            (config.proxy, config.proxy_username, config.proxy_password),
            (None, None, None)
        );
    }

    /// Off a team server's private network, Chrome starts as it is, its
    /// command left alone.
    #[test]
    fn chrome_is_started_headless_over_a_pipe_with_no_port() {
        let chrome = Path::new("/opt/rovik/browser/155.0.8059.39/chrome");
        let config = launch_config(chrome, None, None);
        assert_eq!(config.chrome_path.as_deref(), chrome.to_str());
        assert!(config.headless);
        assert_eq!(config.pipe, cfg!(unix));
        assert!(!config.live_session);
        assert!(config.command_hook.is_none());
        assert!(
            !config
                .extra_args
                .iter()
                .any(|arg| arg.starts_with("--remote-debugging-port")),
            "{:?}",
            config.extra_args
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_ports_a_process_listens_on_are_found() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(listening_ports(std::process::id()).unwrap().contains(&port));

        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        assert!(crate::listening::process_tree(std::process::id())
            .unwrap()
            .contains(&child.id()));
        assert_eq!(listening_ports(child.id()).unwrap(), Vec::<u16>::new());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn the_chromium_snap_is_recognized_by_its_launcher_path() {
        // /snap/bin/chromium resolves to /usr/bin/snap, an ELF file.
        assert!(is_snap_chromium(Path::new("/snap/bin/chromium")));
        assert!(!is_snap_chromium(Path::new("/opt/google/chrome/chrome")));
    }

    /// Against a real Chrome, on the systems where it runs over a pipe.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    mod real_chrome {
        use super::*;

        /// Serves `html` at `/` on 127.0.0.1, until the test ends.
        async fn serve(html: &'static str) -> String {
            serve_on(std::net::TcpListener::bind("127.0.0.1:0").unwrap(), html).await
        }

        /// Serves `html` at `/` on `listener`, until the test ends: its URL.
        async fn serve_on(listener: std::net::TcpListener, html: &'static str) -> String {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            listener.set_nonblocking(true).unwrap();
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            let url = format!("http://{}/", listener.local_addr().unwrap());
            tokio::spawn(async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    tokio::spawn(async move {
                        let mut request = Vec::new();
                        let mut buffer = [0; 4096];
                        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                            match socket.read(&mut buffer).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => request.extend_from_slice(&buffer[..n]),
                            }
                        }
                        let (status, body) = if request.starts_with(b"GET / ") {
                            ("200 OK", html)
                        } else {
                            ("404 Not Found", "")
                        };
                        let response = format!(
                            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = socket.write_all(response.as_bytes()).await;
                    });
                }
            });
            url
        }

        /// `pid` and every process descended from it.
        #[cfg(target_os = "macos")]
        fn process_tree(pid: u32) -> Vec<u32> {
            let output = std::process::Command::new("ps")
                .args(["-A", "-o", "pid=,ppid="])
                .output()
                .unwrap();
            let mut children = std::collections::HashMap::<u32, Vec<u32>>::new();
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let mut fields = line.split_whitespace().map(str::parse::<u32>);
                if let (Some(Ok(child)), Some(Ok(parent))) = (fields.next(), fields.next()) {
                    children.entry(parent).or_default().push(child);
                }
            }
            let mut tree = vec![pid];
            let mut next = 0;
            while next < tree.len() {
                tree.extend(children.get(&tree[next]).into_iter().flatten().copied());
                next += 1;
            }
            tree
        }

        /// The TCP sockets listening in `pids`.
        #[cfg(target_os = "macos")]
        fn listening_tcp_sockets(pids: &[u32]) -> Vec<String> {
            let pids: Vec<String> = pids.iter().map(u32::to_string).collect();
            let output = std::process::Command::new("lsof")
                .args(["-nP", "-a", "-iTCP", "-sTCP:LISTEN", "-p", &pids.join(",")])
                .output()
                .unwrap();
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .skip(1)
                .map(str::to_owned)
                .collect()
        }

        /// The app's own copy, for real: the pinned browser downloaded from
        /// Google into the app's data folder and checked, then the browser
        /// the tools start and read a page with.
        #[test]
        #[ignore = "downloads the pinned browser, about 200 MB"]
        fn the_app_downloads_the_pinned_browser_and_the_tools_use_it() {
            let data = crate::testing::temp_dir();
            // Run on its own (`--ignored --exact`): the app's data folder
            // is found from the environment.
            let folder = if cfg!(target_os = "macos") {
                "HOME"
            } else {
                "XDG_DATA_HOME"
            };
            std::env::set_var(folder, &data.0);
            crate::rt::block_on(async {
                let mut told = (0, 0);
                let path = crate::pinned_browser::install_for_app(|got, of| told = (got, of))
                    .await
                    .unwrap();
                assert!(path.starts_with(&data.0), "{}", path.display());
                assert!(told.0 > 0 && told.0 == told.1, "{told:?}");
                if !Path::new(crate::pinned_browser::SERVER_ROOT).exists() {
                    assert_eq!(chrome_path(), Some(path.clone()));
                }
                // Installed, it's found and not downloaded again.
                let mut again = false;
                let found = crate::pinned_browser::install_for_app(|_, _| again = true)
                    .await
                    .unwrap();
                assert_eq!((found, again), (path, false));

                let url = serve("<!doctype html><title>Pinned</title><p>The app's copy.</p>").await;
                let state = BrowserState::default();
                let page = browse_url(&state, url, None).await.unwrap();
                assert_eq!(page.title, "Pinned");
                state.shutdown().await;
            });
        }

        /// A private network for one test, where pasta is installed: as a
        /// team server's daemon makes one for its user (D20).
        #[cfg(target_os = "linux")]
        fn private_network() -> Option<(crate::testing::TempDir, Arc<Network>)> {
            crate::overlay::confine::network::for_test(Path::new("/bin/true"), &[], None)
                .map(|(dir, network)| (dir, Arc::new(network)))
        }

        /// A port Chrome opens from a flag Rovik didn't give, as a Chrome
        /// flags file would add it, is found: in the machine's network, and
        /// in a team server user's private network, where Chrome starts
        /// there.
        #[cfg(target_os = "linux")]
        #[test]
        fn a_port_from_a_flag_rovik_did_not_give_is_found() {
            let Some(chrome) = chrome_path().filter(|chrome| !is_snap_chromium(chrome)) else {
                eprintln!("no Chrome or Chromium outside a snap here, skipping");
                return;
            };
            let dir = crate::testing::temp_dir();
            let wrapper = dir.0.join("chrome-with-a-port");
            let script = format!(
                "#!/bin/sh\nexec '{}' --remote-debugging-port=0 \"$@\"\n",
                chrome.display()
            );
            std::fs::write(&wrapper, script).unwrap();
            std::fs::set_permissions(
                &wrapper,
                std::os::unix::fs::PermissionsExt::from_mode(0o755),
            )
            .unwrap();
            // The machine's network, then a private one where it's made.
            let private = private_network();
            let mut networks = vec![None];
            networks.extend(private.as_ref().map(|(_, network)| Some(network)));
            for (n, network) in networks.into_iter().enumerate() {
                let profile = dir.0.join(format!("profile-{n}"));
                crate::rt::block_on(async {
                    let mut config = launch_config(&wrapper, None, network);
                    config.user_data_dir = Some(profile.to_string_lossy().into_owned());
                    let browser = Browser::launch_with_config(config).await.unwrap();
                    let pid = browser.process_id().unwrap();
                    // Chrome names the port it chose in the profile, soon
                    // after it answers on the pipe.
                    let mut found = (None, Vec::new());
                    for _ in 0..100 {
                        let named = std::fs::read_to_string(profile.join("DevToolsActivePort"))
                            .ok()
                            .and_then(|text| text.lines().next()?.parse::<u16>().ok());
                        found = (named, listening_ports(pid).unwrap());
                        if found.0.is_some() && !found.1.is_empty() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    let (named, listening) = found;
                    let named = named.expect("Chrome named its port");
                    let private = network.is_some();
                    assert_eq!(listening, vec![named], "in a private network: {private}");
                    browser.close().await.unwrap();
                });
            }
        }

        /// On a team server, the agent's browser opens the page of a server
        /// in the user's private network, as a dev server the agent left
        /// running there serves it, and not the page another user's server
        /// serves on the same port of the server's own loopback (D20).
        #[cfg(target_os = "linux")]
        #[test]
        fn the_browser_opens_the_users_own_server_in_their_private_network() {
            if chrome_path().is_none() {
                eprintln!("no Chrome or Chromium here, skipping");
                return;
            }
            let Some((_dir, network)) = private_network() else {
                return;
            };
            let theirs = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = theirs.local_addr().unwrap().port();
            let ours = network
                .listen_on(port)
                .expect("the port is free in the network");
            crate::rt::block_on(async {
                serve_on(theirs, "<!doctype html><title>Another user's</title>").await;
                for listener in ours {
                    serve_on(
                        listener,
                        "<!doctype html><title>The user's own</title>\
                         <main><p>Served in their network.</p></main>",
                    )
                    .await;
                }
                let live = launch(None, Some(network.clone())).await.unwrap();
                open(&live.page, &format!("http://localhost:{port}/"))
                    .await
                    .unwrap();
                let page = readable(&live.page, 1_000).await.unwrap();
                assert_eq!(page.title, "The user's own");
                assert_eq!(page.text, "Served in their network.");
                live.browser.close().await.unwrap();
            });
        }

        /// The tools against a real Chrome, when one is installed: a page's
        /// text, and a screenshot of it in the chat's attachments. Chrome runs
        /// over a pipe, so it listens on no TCP port that another user of the
        /// machine could connect to (D18).
        #[test]
        fn browse_url_and_browser_screenshot_drive_chrome_over_a_pipe() {
            if chrome_path().is_none() {
                eprintln!("no Chrome or Chromium here, skipping");
                return;
            }
            crate::rt::block_on(async {
                let url = serve(
                    "<!doctype html><title>Rovik fixture</title>\
                     <main><h1>Pipe</h1><p>Read over the DevTools pipe.</p></main>",
                )
                .await;
                let dir = crate::testing::temp_dir();
                let db = crate::chat_db::DbState::open(&dir.0).unwrap();
                let state = BrowserState::default();

                let page = browse_url(&state, url.clone(), None).await.unwrap();
                assert_eq!(page.title, "Rovik fixture");
                assert!(
                    page.text.contains("Read over the DevTools pipe."),
                    "{}",
                    page.text
                );

                let shot = browser_screenshot_inner(&state, &db, None, None)
                    .await
                    .unwrap();
                assert_eq!(shot.url, url);
                let (mime, jpeg) =
                    crate::chat_db::attachments::read(db.data_dir(), &shot.attachment_id).unwrap();
                assert_eq!(mime, "image/jpeg");
                assert!(jpeg.starts_with(&[0xFF, 0xD8, 0xFF]));

                let webdriver = state
                    .with_page(
                        |page| async move {
                            page.evaluate_sync::<bool>("navigator.webdriver")
                                .await
                                .map_err(|e| e.to_string())
                        },
                        None,
                    )
                    .await
                    .unwrap();
                assert!(!webdriver);

                let pid = state.chrome_pid().await.expect("Chrome is running");
                #[cfg(target_os = "linux")]
                let listening = listening_ports(pid).unwrap();
                #[cfg(target_os = "macos")]
                let listening = listening_tcp_sockets(&process_tree(pid));
                assert!(listening.is_empty(), "Chrome listens on TCP: {listening:?}");
                state.shutdown().await;
            });
        }
    }
}
