//! Web tools for the agent, backed by an embedded headless Chrome (eoka).
//!
//! - One browser + one tab, launched lazily on the first web tool call and
//!   shut down after `IDLE_SHUTDOWN` without use, so Chrome only costs memory
//!   while the agent is actually browsing.
//! - Stealth browser (`StealthConfig::default`: evasions, patched binary,
//!   human-like input), plus a Cloudflare-challenge solver that runs before
//!   every read and between the steps of `browser_automate`, so bot checks
//!   don't block the agent.
//! - `browse_url` / `web_search` / `browser_screenshot` are read-only:
//!   navigate, read, screenshot. `browser_automate` may additionally click,
//!   type and press keys (that's what solves challenges and drives
//!   JavaScript-heavy pages), so it's blocked in Plan mode and described as
//!   acting on the page — a prompt-injected page must never be followed.
//! - Only http(s) URLs. `file://` would let the model read any local file,
//!   bypassing the project-root sandbox the file tools enforce.

use crate::eoka_automation::{
    get_cloudflare_captcha_watcher, run_steps, settle_cloudflare, Capture, Step,
};
use crate::agent_run::StopSignal;
use eoka::{Browser, Page, StealthConfig};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::State;
use tokio::sync::Mutex;

const IDLE_SHUTDOWN: Duration = Duration::from_secs(5 * 60);
const IDLE_CHECK_EVERY: Duration = Duration::from_secs(30);
/// Upper bound for any single tool call, including a cold Chrome start.
const OP_TIMEOUT: Duration = Duration::from_secs(60);
/// Automation runs drive whole flows (navigations, waits, challenge
/// solving), so they get longer than a single read.
const AUTOMATE_TIMEOUT: Duration = Duration::from_secs(180);
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

    /// Same, with an explicit upper bound (automation runs longer reads).
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

            if inner.live.is_none() {
                inner.live = Some(launch().await?);
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
        tauri::async_runtime::spawn(async move {
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

async fn launch() -> Result<Live, String> {
    let chrome = eoka::stealth::find_chrome().map_err(|_| {
        "Chrome or Chromium was not found. Install Google Chrome or Chromium to use web tools."
            .to_string()
    })?;

    let mut config = StealthConfig::default();
    config.headless = true;
    config.viewport_width = VIEWPORT.0;
    config.viewport_height = VIEWPORT.1;
    config.chrome_path = Some(chrome.to_string_lossy().into_owned());

    // eoka puts its throwaway profile in /tmp and reads Chrome's DevTools
    // port from a file inside it. Snap Chromium has a private /tmp, so that
    // file would never appear. Use a profile inside the snap's own writable
    // area instead, wiped on every launch so each session starts clean.
    if let Some(profile) = snap_profile_dir(&chrome) {
        let _ = std::fs::remove_dir_all(&profile);
        config.user_data_dir = Some(profile.to_string_lossy().into_owned());
    }

    let browser = Browser::launch_with_config(config)
        .await
        .map_err(|e| format!("Failed to start Chrome ({}): {e}", chrome.display()))?;
    let page = browser
        .new_blank_page()
        .await
        .map_err(|e| format!("Failed to open a browser tab: {e}"))?;

    Ok(Live { browser, page })
}

/// `Some(profile dir)` when `chrome` is (or wraps) the Chromium snap.
fn snap_profile_dir(chrome: &Path) -> Option<PathBuf> {
    if !cfg!(target_os = "linux") || !is_snap_chromium(chrome) {
        return None;
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("snap/chromium/common/tauri-ide-browser"))
}

fn is_snap_chromium(chrome: &Path) -> bool {
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

fn truncate_chars(text: &str, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((cut, _)) => (text[..cut].to_string(), true),
        None => (text.to_string(), false),
    }
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
#[tauri::command]
pub async fn web_search(
    state: State<'_, BrowserState>,
    query: String,
    max_results: Option<usize>,
) -> Result<Vec<SearchHit>, String> {
    web_search_inner(&state, query, max_results, None).await
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
#[tauri::command]
pub async fn browse_url(
    state: State<'_, BrowserState>,
    url: String,
    max_chars: Option<usize>,
) -> Result<BrowsedPage, String> {
    browse_url_inner(&state, url, max_chars, None).await
}

/// `browse_url` for an agent run: Stop ends the wait at once.
pub(crate) async fn browse_url_inner(
    state: &BrowserState,
    url: String,
    max_chars: Option<usize>,
    stop: Option<&StopSignal>,
) -> Result<BrowsedPage, String> {
    let url = validate_url(&url)?;
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
pub struct Screenshot {
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
#[tauri::command]
pub async fn browser_screenshot(
    state: State<'_, BrowserState>,
    db: State<'_, crate::chat_db::DbState>,
    url: Option<String>,
) -> Result<Screenshot, String> {
    browser_screenshot_inner(&state, &db, url, None).await
}

/// `browser_screenshot` for an agent run: Stop ends the wait at once.
pub(crate) async fn browser_screenshot_inner(
    state: &BrowserState,
    db: &crate::chat_db::DbState,
    url: Option<String>,
    stop: Option<&StopSignal>,
) -> Result<Screenshot, String> {
    let url = url
        .filter(|u| !u.trim().is_empty())
        .map(|u| validate_url(&u))
        .transpose()?;

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

    let attachment_id = crate::chat_db::attachments::store(&db, bytes, "image/jpeg").await?;
    Ok(Screenshot {
        url: current,
        title,
        attachment_id,
        width: VIEWPORT.0,
        height: VIEWPORT.1,
    })
}

/*
 * ---------------------------------------------------------------------------
 * Scripted automation: agent-driven steps with a Cloudflare watcher
 * ---------------------------------------------------------------------------
 */

/// One agent-driven step: a small, serializable subset of `Step`.
/// Everything here is data (no callbacks), validated and capped before it
/// runs. New `Step` variants can grow this list later.
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AgentStep {
    Goto {
        url: String,
    },
    WaitForNetworkIdle {
        quiet_ms: Option<u64>,
        timeout_ms: Option<u64>,
    },
    WaitForAny {
        selectors: Vec<String>,
        timeout_ms: Option<u64>,
    },
    Wait {
        ms: Option<u64>,
    },
    WaitForText {
        text: String,
        timeout_ms: Option<u64>,
    },
    Click {
        selector: String,
    },
    ClickText {
        text: String,
    },
    Type {
        selector: String,
        text: String,
    },
    Fill {
        selector: String,
        value: String,
    },
    Press {
        key: String,
    },
    CaptureText {
        label: String,
        selector: Option<String>,
    },
    Scroll {
        selector: String,
        y: Option<f64>,
    },
}

/// At most this many steps per call: each step is a navigation, a wait or
/// an interaction, so more belongs in a second call that first looks at
/// what this one captured.
const MAX_AUTOMATE_STEPS: usize = 20;
const MAX_SELECTORS_PER_STEP: usize = 10;

fn capped_selector(raw: &str) -> Result<String, String> {
    let selector = raw.trim();
    if selector.is_empty() {
        return Err("A step has an empty selector.".into());
    }
    if selector.len() > 500 {
        return Err("A step's selector is over 500 characters.".into());
    }
    Ok(selector.to_string())
}

fn capped_text(raw: &str, what: &str) -> Result<String, String> {
    if raw.is_empty() {
        return Err(format!("A step's {what} is empty."));
    }
    if raw.len() > 2000 {
        return Err(format!("A step's {what} is over 2000 characters."));
    }
    Ok(raw.to_string())
}

impl AgentStep {
    fn into_step(self) -> Result<Step, String> {
        match self {
            AgentStep::Goto { url } => Ok(Step::Goto { url: validate_url(&url)? }),
            AgentStep::WaitForNetworkIdle { quiet_ms, timeout_ms } => Ok(Step::WaitForNetworkIdle {
                quiet_ms: quiet_ms.unwrap_or(500).clamp(100, 10_000),
                timeout_ms: timeout_ms.unwrap_or(15_000).clamp(1_000, 60_000),
            }),
            AgentStep::WaitForAny { selectors, timeout_ms } => {
                if selectors.is_empty() || selectors.len() > MAX_SELECTORS_PER_STEP {
                    return Err(format!(
                        "wait_for_any needs 1-{MAX_SELECTORS_PER_STEP} selectors."
                    ));
                }
                let selectors = selectors.iter().map(|s| capped_selector(s)).collect::<Result<_, _>>()?;
                Ok(Step::WaitForAny {
                    selectors,
                    timeout_ms: timeout_ms.unwrap_or(10_000).clamp(1_000, 60_000),
                })
            }
            AgentStep::Wait { ms } => Ok(Step::Wait { ms: ms.unwrap_or(1_000).clamp(0, 30_000) }),
            AgentStep::WaitForText { text, timeout_ms } => Ok(Step::WaitForText {
                text: capped_text(&text, "text")?,
                timeout_ms: timeout_ms.unwrap_or(10_000).clamp(1_000, 60_000),
            }),
            AgentStep::Click { selector } => Ok(Step::Click { selector: capped_selector(&selector)? }),
            AgentStep::ClickText { text } => Ok(Step::ClickText { text: capped_text(&text, "text")? }),
            AgentStep::Type { selector, text } => Ok(Step::Type {
                selector: capped_selector(&selector)?,
                text: capped_text(&text, "text")?,
            }),
            AgentStep::Fill { selector, value } => Ok(Step::Fill {
                selector: capped_selector(&selector)?,
                value: capped_text(&value, "value")?,
            }),
            AgentStep::Press { key } => {
                let key = key.trim();
                if key.is_empty() || key.len() > 50 {
                    return Err("A step's key is empty or over 50 characters.".into());
                }
                Ok(Step::Press { key: key.to_string() })
            }
            AgentStep::CaptureText { label, selector } => {
                let label = label.trim();
                if label.is_empty() || label.len() > 100 {
                    return Err("A step's label is empty or over 100 characters.".into());
                }
                let selector = selector.filter(|s| !s.trim().is_empty()).map(|s| capped_selector(&s)).transpose()?;
                Ok(Step::CaptureText { label: label.to_string(), selector })
            }
            AgentStep::Scroll { selector, y } => Ok(Step::Scroll {
                selector: capped_selector(&selector)?,
                y: y.unwrap_or(500.0).clamp(-10_000.0, 10_000.0),
            }),
        }
    }
}

/// The readable text of whatever the tab shows now, shared by `browse_url`
/// and `browser_automate`.
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

    let (text, truncated) = truncate_chars(&tidy_text(&raw.text), max_chars);
    Ok(BrowsedPage {
        url: raw.url,
        title: raw.title,
        text,
        truncated,
        links: raw.links,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomatedPage {
    pub(crate) url: String,
    pub(crate) title: String,
    pub(crate) text: String,
    pub(crate) truncated: bool,
    pub(crate) captures: Vec<Capture>,
    /// The Cloudflare watcher fired at least once during the run.
    pub(crate) challenge_seen: bool,
}

/// Drives the shared tab through the given steps (up to 20): open pages,
/// wait for content, click, type and capture text. A Cloudflare watcher
/// runs between the steps and solves challenges as they appear; when a
/// challenge won't clear, the call fails instead of reading a blocked page.
/// Returns the final page's text plus everything the capture steps recorded.
#[tauri::command]
pub async fn browser_automate(
    state: State<'_, BrowserState>,
    steps: Vec<AgentStep>,
    max_chars: Option<usize>,
) -> Result<AutomatedPage, String> {
    browser_automate_inner(&state, steps, max_chars, None).await
}

/// `browser_automate` for an agent run: Stop ends the wait at once.
pub(crate) async fn browser_automate_inner(
    state: &BrowserState,
    steps: Vec<AgentStep>,
    max_chars: Option<usize>,
    stop: Option<&StopSignal>,
) -> Result<AutomatedPage, String> {
    if steps.is_empty() {
        return Err("No steps given.".into());
    }
    if steps.len() > MAX_AUTOMATE_STEPS {
        return Err(format!("At most {MAX_AUTOMATE_STEPS} steps per call; split the flow across calls."));
    }
    let steps = steps.into_iter().map(AgentStep::into_step).collect::<Result<Vec<_>, _>>()?;
    let max_chars = max_chars.unwrap_or(DEFAULT_MAX_CHARS).clamp(1_000, 60_000);

    state
        .with_page_timeout(
            |page| async move {
                let (flow, all) = run_steps(&page, &steps, vec![get_cloudflare_captcha_watcher()])
                    .await
                    .map_err(|e| format!("Automation failed: {e}"))?;
                if flow == crate::eoka_automation::Flow::Break {
                    return Err("A bot check blocked the page and could not be solved. Try again later, or a different URL."
                        .to_string());
                }
                let challenge_seen = all.iter().any(|c| c.label == "watcher_triggered:cloudflare_challenge");
                let captures = all.into_iter().filter(|c| !c.label.starts_with("watcher_triggered:")).collect();
                let read = readable(&page, max_chars).await?;
                Ok(AutomatedPage {
                    url: read.url,
                    title: read.title,
                    text: read.text,
                    truncated: read.truncated,
                    captures,
                    challenge_seen,
                })
            },
            AUTOMATE_TIMEOUT,
            stop,
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn step(json: serde_json::Value) -> Result<Step, String> {
        let step: AgentStep =
            serde_json::from_value(json).map_err(|e| format!("parse: {e}"))?;
        step.into_step()
    }

    #[test]
    fn agent_steps_parse_and_validate() {
        // Bare hosts get https, like browse_url.
        assert!(matches!(
            step(json!({"action": "goto", "url": "example.com/docs"})),
            Ok(Step::Goto { url }) if url == "https://example.com/docs"
        ));
        assert!(step(json!({"action": "goto", "url": "file:///etc/passwd"})).is_err());
        assert!(matches!(
            step(json!({"action": "click", "selector": "button.submit"})),
            Ok(Step::Click { .. })
        ));
        assert!(step(json!({"action": "click", "selector": "  "})).is_err());
        assert!(step(json!({"action": "press", "key": ""})).is_err());
        assert!(step(json!({"action": "wait_for_any", "selectors": []})).is_err());
        assert!(step(json!({"action": "capture_text", "label": "", "selector": null})).is_err());
        assert!(step(json!({"action": "fly", "url": "https://x.com"})).is_err());
    }

    #[test]
    fn agent_step_limits_are_clamped() {
        assert!(matches!(
            step(json!({"action": "wait", "ms": 999_999})),
            Ok(Step::Wait { ms }) if ms == 30_000
        ));
        assert!(matches!(
            step(json!({"action": "scroll", "selector": "main"})),
            Ok(Step::Scroll { y, .. }) if y == 500.0
        ));
    }

    /// Stop before a browser call starts: it ends at once without launching
    /// Chrome (no browser needed for this test).
    #[tokio::test]
    async fn stop_before_the_start_runs_nothing() {
        let stop = Arc::new(StopSignal::new());
        stop.set();
        let state = BrowserState::default();
        let err = state
            .with_page(
                |_| async { Ok::<(), String>(()) },
                Some(&stop),
            )
            .await
            .unwrap_err();
        assert_eq!(err, "Stopped by the user.");
    }
}
