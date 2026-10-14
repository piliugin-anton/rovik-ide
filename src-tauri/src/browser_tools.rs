//! Web tools for the agent, backed by an embedded headless Chrome (eoka).
//!
//! - One browser + one tab, launched lazily on the first web tool call and
//!   shut down after `IDLE_SHUTDOWN` without use, so Chrome only costs memory
//!   while the agent is actually browsing.
//! - Stealth browser (`StealthConfig::default`: evasions, patched binary,
//!   human-like input), plus a Cloudflare-challenge solver that runs before
//!   every read, so bot checks don't block the agent.
//! - `browse_url` / `web_search` / `browser_screenshot` are read-only:
//!   navigate, read, screenshot. A prompt-injected page must never be
//!   followed.
//! - Only http(s) URLs. `file://` would let the model read any local file,
//!   bypassing the project-root sandbox the file tools enforce.

use crate::agent_run::StopSignal;
use eoka::{Browser, Page, Result as EokaResult, StealthConfig};
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
    let common = PathBuf::from(std::env::var_os("HOME")?).join("snap/chromium/common");
    let dir = common.join("rovik-ide-browser");
    // The folder's name before the app was renamed: moved over once, so the
    // browser keeps its cookies and logins.
    let legacy = common.join("tauri-ide-browser");
    if !dir.exists() && legacy.is_dir() {
        let _ = std::fs::rename(&legacy, &dir);
    }
    Some(dir)
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

    let (text, truncated) = truncate_chars(&tidy_text(&raw.text), max_chars);
    Ok(BrowsedPage {
        url: raw.url,
        title: raw.title,
        text,
        truncated,
        links: raw.links,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
