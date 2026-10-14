//! The agent's browser tools (browser_tools.rs), on the shared headless
//! Chrome.

use crate::browser_tools::{self, BrowsedPage, BrowserScreenshot, SearchHit};
use crate::core::Core;
use tauri::State;

/// DuckDuckGo's HTML endpoint: server-rendered, no JavaScript needed.
#[tauri::command]
pub async fn web_search(
    core: State<'_, Core>,
    query: String,
    max_results: Option<usize>,
) -> Result<Vec<SearchHit>, String> {
    browser_tools::web_search(&core.browser, query, max_results).await
}

#[tauri::command]
pub async fn browse_url(
    core: State<'_, Core>,
    url: String,
    max_chars: Option<usize>,
) -> Result<BrowsedPage, String> {
    browser_tools::browse_url(&core.browser, url, max_chars).await
}

#[tauri::command]
pub async fn browser_screenshot(
    core: State<'_, Core>,
    url: Option<String>,
) -> Result<BrowserScreenshot, String> {
    browser_tools::browser_screenshot(&core, url).await
}
