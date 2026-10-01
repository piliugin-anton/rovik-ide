//! Web tools for the agent, backed by an embedded headless Chrome (eoka).
//!
//! - One browser + one tab, launched lazily on the first web tool call and
//!   shut down after `IDLE_SHUTDOWN` without use, so Chrome only costs memory
//!   while the agent is actually browsing.
//! - Read-only: navigate, read, screenshot. No clicking, typing or form
//!   submission, so a prompt-injected page can't make the agent act on sites.
//! - Only http(s) URLs. `file://` would let the model read any local file,
//!   bypassing the project-root sandbox the file tools enforce.
//! - eoka's stealth features are all off (`StealthConfig::minimal`): this is
//!   a plain automated Chrome, not a bot-detection bypass.

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
/// Modest viewport: screenshots cost vision tokens roughly by pixel count.
const VIEWPORT: (u32, u32) = (1280, 800);
const DEFAULT_MAX_CHARS: usize = 15_000;
const MAX_LINKS: usize = 40;

const SEARCH_URL: &str = "https://html.duckduckgo.com/html/";

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
    /// on the single tab.
    async fn with_page<T, F, Fut>(&self, op: F) -> Result<T, String>
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

        tokio::time::timeout(OP_TIMEOUT, run)
            .await
            .map_err(|_| format!("Browser operation timed out after {}s", OP_TIMEOUT.as_secs()))?
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

    let mut config = StealthConfig::minimal();
    config.headless = true; // minimal() is headed by default
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
async fn open(page: &Page, url: &str) -> Result<(), String> {
    page.goto(url)
        .await
        .map_err(|e| format!("Could not open {url}: {e}"))?;

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
    title: String,
    url: String,
    snippet: String,
}

/// DuckDuckGo's HTML endpoint: server-rendered, no JavaScript needed.
#[tauri::command]
pub async fn web_search(
    state: State<'_, BrowserState>,
    query: String,
    max_results: Option<usize>,
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
        })
        .await
}

#[derive(Serialize, Deserialize)]
pub struct PageLink {
    text: String,
    url: String,
}

#[derive(Serialize)]
pub struct BrowsedPage {
    url: String,
    title: String,
    text: String,
    truncated: bool,
    links: Vec<PageLink>,
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
    let url = validate_url(&url)?;
    let max_chars = max_chars.unwrap_or(DEFAULT_MAX_CHARS).clamp(1_000, 60_000);

    state
        .with_page(|page| async move {
            open(&page, &url).await?;

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
        })
        .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Screenshot {
    url: String,
    title: String,
    /// Stored in the chat database's attachments; shown by URL, and only
    /// base64-encoded when a request to a vision model is built.
    attachment_id: String,
    width: u32,
    height: u32,
}

/// Screenshots the viewport, opening `url` first when given (otherwise the
/// page currently open in the agent's tab).
#[tauri::command]
pub async fn browser_screenshot(
    state: State<'_, BrowserState>,
    db: State<'_, crate::chat_db::DbState>,
    url: Option<String>,
) -> Result<Screenshot, String> {
    let url = url
        .filter(|u| !u.trim().is_empty())
        .map(|u| validate_url(&u))
        .transpose()?;

    let (current, title, bytes) = state
        .with_page(|page| async move {
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
        })
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
