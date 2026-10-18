//! The browser for the web tools (D19): the app's own copy of the Chrome
//! for Testing Rovik pins, downloaded in the background when the app starts
//! and it isn't there. `src/lib/browserStartup.ts` asks for it and shows
//! the download's progress; `rovik_core::pinned_browser` does the work.

use rovik_core::pinned_browser::{self, Build, NAME, VERSION};
use serde::Serialize;
use tauri::ipc::Channel;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserStatus {
    pub name: &'static str,
    pub version: &'static str,
    /// Where it's installed, if it is: this machine's server install or the
    /// app's own copy.
    pub path: Option<String>,
    /// The download's size in bytes, when Google builds it for this system.
    pub size: Option<u64>,
}

/// Whether the pinned browser is installed, and how big its download is.
#[tauri::command]
pub async fn browser_status() -> BrowserStatus {
    BrowserStatus {
        name: NAME,
        version: VERSION,
        path: pinned_browser::installed().map(|path| path.to_string_lossy().into_owned()),
        size: Build::here().map(|build| build.size),
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BrowserProgress {
    pub downloaded: u64,
    pub total: u64,
}

/// Downloads the pinned browser into the app's data folder, telling
/// `on_progress` how far it's got every 256 KiB and at the end. Its
/// executable's path.
#[tauri::command]
pub async fn browser_install(on_progress: Channel<BrowserProgress>) -> Result<String, String> {
    let mut told = 0u64;
    let path = pinned_browser::install_for_app(|downloaded, total| {
        if downloaded - told >= 256 * 1024 || downloaded == total {
            told = downloaded;
            let _ = on_progress.send(BrowserProgress { downloaded, total });
        }
    })
    .await?;
    Ok(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    /// The browser tools look for the app's copy in the folder named after
    /// the app's identifier, without asking Tauri.
    #[test]
    fn the_app_and_the_browser_tools_agree_on_its_folder() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(
            config["identifier"],
            rovik_core::pinned_browser::APP_ID,
            "pinned_browser::app_root() names the app's data folder"
        );
    }
}
