//! `rovik-agentd doctor`: what this account and server lack for the daemon,
//! a check a line. Problems stop the daemon from starting; warnings name
//! what won't work (the browser tools without Chrome, CLM without FUSE).

use crate::auth::Tokens;
use crate::cli::{Options, VERSION};
use crate::config::Config;
use crate::home::Home;
use crate::host;
use rovik_protocol::PROTOCOL;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

pub fn doctor(options: Options) -> Result<(), String> {
    let home = Home::locate(options.data)?;
    let socket = options.socket.unwrap_or_else(|| home.socket());
    let mut report = Report::default();
    println!("rovik-agentd {VERSION} (protocol {PROTOCOL})");

    let folders = home.problems();
    if folders.is_empty() {
        let names: Vec<String> = home
            .folders()
            .iter()
            .map(|f| f.display().to_string())
            .collect();
        report.ok(format!(
            "{}: this user's alone (made on the first start if missing)",
            names.join(" and ")
        ));
    }
    for problem in folders {
        report.problem(problem);
    }

    match Config::read(&home.config_file()).and_then(|c| c.level().and(Tokens::new(&c.tokens))) {
        Ok(tokens) if tokens.len() == 0 => {
            report.warning("config.toml names no computer: none can connect yet".into())
        }
        Ok(tokens) => report.ok(format!("config.toml names {} computer(s)", tokens.len())),
        Err(e) => report.problem(e),
    }

    if answers(&socket) {
        report.ok(format!("a daemon answers at {}", socket.display()));
    } else {
        report.warning(format!("no daemon answers at {}", socket.display()));
    }

    let fuse = rovik_core::vfs::driver::status();
    match fuse.detail {
        None => report.ok(format!(
            "virtual files: {} {}",
            fuse.driver,
            fuse.version.unwrap_or_default()
        )),
        Some(why) => report.warning(format!(
            "virtual files: {why} CLM chats fall back to trimming."
        )),
    }

    match rovik_core::browser_tools::chrome_path() {
        Some(chrome) => report.ok(format!("Chrome: {}", chrome.display())),
        None => report.warning("no Chrome or Chromium: the browser tools won't work".into()),
    }

    if host::user_namespaces(Path::new("/proc/sys")) {
        report.ok("unprivileged user namespaces are allowed".into());
    } else {
        report.warning(
            "unprivileged user namespaces are off: Chrome runs without its sandbox".into(),
        );
    }

    match report.problems {
        0 => Ok(()),
        n => Err(format!("{n} problem(s) to fix before the daemon can start")),
    }
}

#[derive(Default)]
struct Report {
    problems: usize,
}

impl Report {
    fn ok(&mut self, line: String) {
        println!("  ok       {line}");
    }

    fn warning(&mut self, line: String) {
        println!("  warning  {line}");
    }

    fn problem(&mut self, line: String) {
        self.problems += 1;
        println!("  problem  {line}");
    }
}

/// Whether a daemon at `socket` answers `/health`.
fn answers(socket: &Path) -> bool {
    let Ok(mut stream) = std::os::unix::net::UnixStream::connect(socket) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let request = "GET /health HTTP/1.1\r\nHost: rovik-agentd\r\nConnection: close\r\n\r\n";
    let mut answer = String::new();
    stream.write_all(request.as_bytes()).is_ok()
        && stream.read_to_string(&mut answer).is_ok()
        && answer.starts_with("HTTP/1.1 200")
}
