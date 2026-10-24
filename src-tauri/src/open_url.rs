// src-tauri/src/open_url.rs
//
// Opens a link from the chat in the system's default browser. The webview
// ignores target="_blank", and navigating it away would replace the IDE.
// A page a remote chat's command opens on its team server opens the same
// way, once its port is forwarded here (rovik-client's remote/pages.rs).

use std::process::{Command, Stdio};

/// Only web and mail links: anything else (file:, javascript:, custom
/// schemes) could start a local program through the OS's opener.
fn is_allowed(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    ["http://", "https://", "mailto:"]
        .iter()
        .any(|p| lower.starts_with(p))
}

#[tauri::command]
pub async fn open_url(url: String) -> Result<(), String> {
    open(&url)
}

/// Opens `url`, a web or mail link, in the system's default browser.
pub fn open(url: &str) -> Result<(), String> {
    if !is_allowed(url) {
        return Err(format!("not a web link: {url}"));
    }
    let mut cmd = opener(url);
    rovik_core::process::no_window(&mut cmd);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("couldn't open the browser: {e}"))?;
    // Reaped off this thread: xdg-open may stay around until the browser
    // exits, and an unwaited child stays a zombie on Unix.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(target_os = "macos")]
fn opener(url: &str) -> Command {
    let mut cmd = Command::new("/usr/bin/open");
    cmd.arg(url);
    cmd
}

// Not `cmd /c start`: cmd would interpret `&` and `^` in the URL.
#[cfg(windows)]
fn opener(url: &str) -> Command {
    let mut cmd = Command::new("rundll32");
    cmd.args(["url.dll,FileProtocolHandler", url]);
    cmd
}

#[cfg(not(any(target_os = "macos", windows)))]
fn opener(url: &str) -> Command {
    let mut cmd = rovik_core::process::command("xdg-open");
    cmd.arg(url);
    cmd
}

#[cfg(test)]
mod tests {
    use super::is_allowed;

    #[test]
    fn only_web_and_mail_links() {
        assert!(is_allowed("https://example.com/a?b=1&c=2"));
        assert!(is_allowed("HTTP://example.com"));
        assert!(is_allowed("mailto:a@b.c"));
        assert!(!is_allowed("file:///etc/passwd"));
        assert!(!is_allowed("javascript:alert(1)"));
        assert!(!is_allowed("vscode://open"));
        assert!(!is_allowed("/relative"));
    }
}
