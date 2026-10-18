//! Chrome over its DevTools pipe (`StealthConfig::pipe`).
//!
//! The fixture tests stand in for Chrome with a shell script and need no
//! browser. The ignored tests need Chrome:
//! `cargo test --test pipe -- --ignored`.

#![cfg(unix)]

use eoka::{Browser, StealthConfig};
use std::{
    collections::HashMap,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("eoka-pipe-{:08x}", fastrand::u32(..)));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A stand-in for Chrome: `body` runs for a launch, after the version probe.
fn fixture_chrome(root: &Path, body: &str) -> PathBuf {
    let path = root.join("chrome-fixture");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'Chrome 155.0.8059.39\\n'; exit 0; fi\n{body}\n"
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn fixture_config(root: &Path, chrome: &Path) -> StealthConfig {
    StealthConfig {
        chrome_path: Some(chrome.to_string_lossy().into_owned()),
        runtime_dir: Some(root.join("runtime").to_string_lossy().into_owned()),
        patch_binary: false,
        pipe: true,
        cdp_timeout: 2,
        ..Default::default()
    }
}

/// The reply to the transport's first command, `Browser.getVersion` (id 1).
const VERSION_REPLY: &str = r#"{"id":1,"result":{"protocolVersion":"1.3","product":"Fixture/155.0.8059.39","revision":"","userAgent":"Fixture","jsVersion":""}}"#;

#[tokio::test]
async fn chrome_gets_the_pipe_as_fds_3_and_4_and_no_port() {
    let root = Directory::new();
    let evidence = root.0.join("launch.txt");
    // Read one byte of the first command on fd 3, answer it on fd 4, record
    // what Chrome was given, and exit.
    let chrome = fixture_chrome(
        &root.0,
        &format!(
            "{{ [ -p /dev/fd/3 ] && echo fd3-pipe; [ -p /dev/fd/4 ] && echo fd4-pipe; printf '%s\\n' \"$@\"; }} > '{evidence}'\n\
             dd bs=1 count=1 <&3 >/dev/null 2>&1 && echo fd3-read >> '{evidence}'\n\
             printf '%s\\000' '{VERSION_REPLY}' >&4\n",
            evidence = evidence.display()
        ),
    );
    let browser = Browser::launch_with_config(fixture_config(&root.0, &chrome))
        .await
        .expect("the fixture answers on its pipe");
    assert!(browser.owns_process());
    assert!(browser.process_id().is_some());
    browser.close().await.unwrap();

    let evidence = std::fs::read_to_string(evidence).unwrap();
    let lines: Vec<&str> = evidence.lines().collect();
    for expected in [
        "fd3-pipe",
        "fd4-pipe",
        "fd3-read",
        "--remote-debugging-pipe",
    ] {
        assert!(lines.contains(&expected), "{expected} missing: {lines:?}");
    }
    assert!(
        !lines
            .iter()
            .any(|arg| arg.starts_with("--remote-debugging-port")),
        "a pipe launch must not open a port: {lines:?}"
    );
    assert!(
        !root.0.join("runtime").read_dir().unwrap().any(|_| true),
        "close must remove the owned runtime"
    );
}

#[tokio::test]
async fn a_chrome_that_exits_is_reported_with_its_status_and_stderr() {
    let root = Directory::new();
    let chrome = fixture_chrome(
        &root.0,
        "printf 'fixture: no usable display\\n' >&2\nexit 7",
    );
    let error = Browser::launch_with_config(fixture_config(&root.0, &chrome))
        .await
        .err()
        .expect("the fixture never answers");
    let message = error.to_string();
    assert!(
        message.contains("did not answer on its DevTools pipe"),
        "{message}"
    );
    assert!(message.contains('7'), "exit status missing: {message}");
    assert!(message.contains("fixture: no usable display"), "{message}");
    assert!(
        !root.0.join("runtime").read_dir().unwrap().any(|_| true),
        "a failed launch must remove the owned runtime"
    );
}

#[tokio::test]
async fn a_chrome_that_never_answers_times_out_and_is_stopped() {
    let root = Directory::new();
    let pid_file = root.0.join("pid");
    let chrome = fixture_chrome(
        &root.0,
        &format!("echo $$ > '{}'\nexec sleep 30", pid_file.display()),
    );
    let started = Instant::now();
    let error = Browser::launch_with_config(fixture_config(&root.0, &chrome))
        .await
        .err()
        .expect("the fixture never answers");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the command timeout must bound readiness"
    );
    assert!(
        error
            .to_string()
            .contains("did not answer on its DevTools pipe"),
        "{error}"
    );
    let pid: i32 = std::fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: signal 0 only checks whether the process exists.
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    assert!(!alive, "the silent fixture must be stopped");
}

/// Real Chrome, if installed.
fn chrome_available() -> bool {
    eoka::stealth::find_chrome().is_ok()
}

/// `pid` and every process descended from it.
fn process_tree(pid: u32) -> Vec<u32> {
    let output = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid="])
        .output()
        .unwrap();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split_whitespace().map(|field| field.parse::<u32>());
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
#[cfg(target_os = "linux")]
fn listening_tcp_sockets(pids: &[u32]) -> Vec<String> {
    let mut listening = std::collections::HashSet::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(table) = std::fs::read_to_string(table) else {
            continue;
        };
        for row in table.lines().skip(1) {
            let columns: Vec<&str> = row.split_whitespace().collect();
            // State 0A is LISTEN; column 9 is the socket's inode.
            if columns.get(3) == Some(&"0A") {
                if let Some(inode) = columns.get(9) {
                    listening.insert(format!("socket:[{inode}]"));
                }
            }
        }
    }
    let mut found = Vec::new();
    for pid in pids {
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if let Ok(target) = std::fs::read_link(fd.path()) {
                let target = target.to_string_lossy().into_owned();
                if listening.contains(&target) {
                    found.push(format!("pid {pid}: {target}"));
                }
            }
        }
    }
    found
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
#[ignore = "requires Chrome"]
async fn a_pipe_browser_listens_on_no_tcp_port() {
    if !chrome_available() {
        eprintln!("Chrome not found, skipping test");
        return;
    }
    for pipe in [false, true] {
        let browser = Browser::launch_with(|config| config.pipe = pipe)
            .await
            .unwrap();
        // Renderers and helpers are running by the time a page exists.
        let page = browser
            .new_page("data:text/html,<p>ready</p>")
            .await
            .unwrap();
        assert_eq!(page.evaluate_sync::<i32>("1 + 1").await.unwrap(), 2);
        let tree = process_tree(browser.process_id().unwrap());
        let listening = listening_tcp_sockets(&tree);
        browser.close().await.unwrap();
        if pipe {
            assert!(
                listening.is_empty(),
                "Chrome over a pipe must not listen on TCP: {listening:?}"
            );
        } else {
            // The check sees the DevTools port, so its silence above means
            // something.
            assert!(
                !listening.is_empty(),
                "the port launch's DevTools port was not found in {tree:?}"
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn navigator_webdriver_is_false_over_the_pipe_without_evasions() {
    if !chrome_available() {
        eprintln!("Chrome not found, skipping test");
        return;
    }
    // A live session injects no evasion script: this is Chrome's own value,
    // which the pipe switch would set if eoka's launch flags didn't clear it.
    let browser = Browser::launch_with(|config| {
        config.pipe = true;
        config.live_session = true;
    })
    .await
    .unwrap();
    let page = browser
        .new_page("data:text/html,<p>native</p>")
        .await
        .unwrap();
    assert!(!page
        .evaluate_sync::<bool>("navigator.webdriver")
        .await
        .unwrap());
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn the_pipe_reaches_chrome_through_a_wrapper_script() {
    let Ok(chrome) = eoka::stealth::find_chrome() else {
        eprintln!("Chrome not found, skipping test");
        return;
    };
    // As Ubuntu's /usr/bin/chromium-browser and the snap's launcher do.
    let root = Directory::new();
    let wrapper = root.0.join("chromium-wrapper");
    std::fs::write(
        &wrapper,
        format!("#!/bin/sh\nexec '{}' \"$@\"\n", chrome.display()),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let browser = Browser::launch_with(|config| {
        config.pipe = true;
        config.chrome_path = Some(wrapper.to_string_lossy().into_owned());
        // A wrapper can't be patched; the snap's profile isn't either.
        config.patch_binary = false;
    })
    .await
    .unwrap();
    let page = browser
        .new_page("data:text/html,<p>wrapped</p>")
        .await
        .unwrap();
    assert_eq!(page.evaluate_sync::<i32>("6 * 7").await.unwrap(), 42);
    browser.close().await.unwrap();
}

/// Signals bot detectors read, as one page sees them.
const DETECTION_SIGNALS: &str = r#"(() => {
    let consoleSerialized = false;
    const error = new Error();
    Object.defineProperty(error, 'stack', { get() { consoleSerialized = true; return ''; } });
    console.debug(error);
    const getter = Object.getOwnPropertyDescriptor(Navigator.prototype, 'webdriver')?.get;
    return {
        webdriver: navigator.webdriver,
        webdriverInNavigator: 'webdriver' in navigator,
        webdriverOwnProperty: Object.getOwnPropertyNames(navigator).includes('webdriver'),
        webdriverGetter: getter ? Function.prototype.toString.call(getter) : null,
        automationMarkers: Object.keys(window).filter((key) => /cdc_|webdriver|selenium/.test(key)),
        consoleSerialized,
        chrome: typeof window.chrome,
        chromeRuntime: !!(window.chrome && window.chrome.runtime),
        plugins: navigator.plugins.length,
        languages: navigator.languages,
        userAgent: navigator.userAgent,
        platform: navigator.platform,
        timeZone: Intl.DateTimeFormat().resolvedOptions().timeZone,
        pointerFine: matchMedia('(pointer: fine)').matches,
        hover: matchMedia('(hover: hover)').matches,
        screen: [screen.width, screen.height],
        outer: [outerWidth, outerHeight],
    };
})()"#;

#[tokio::test]
#[ignore = "requires Chrome"]
async fn stealth_signals_are_the_same_over_the_pipe_and_the_port() {
    if !chrome_available() {
        eprintln!("Chrome not found, skipping test");
        return;
    }
    let mut seen = Vec::new();
    for pipe in [false, true] {
        let browser = Browser::launch_with(|config| {
            config.pipe = pipe;
            // Fixed, so the two launches claim the same identity.
            config.user_agent = Some(
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36".into(),
            );
            config.timezone = Some("Europe/Berlin".into());
        })
        .await
        .unwrap();
        let page = browser
            .new_page("data:text/html,<title>signals</title><p>signals</p>")
            .await
            .unwrap();
        let signals: serde_json::Value = page.evaluate_sync(DETECTION_SIGNALS).await.unwrap();
        browser.close().await.unwrap();
        assert_eq!(signals["webdriver"], false, "{signals}");
        assert_eq!(signals["consoleSerialized"], false, "{signals}");
        assert_eq!(
            signals["automationMarkers"],
            serde_json::json!([]),
            "{signals}"
        );
        seen.push(signals);
    }
    assert_eq!(seen[0], seen[1], "the pipe changed what pages see");
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn pages_tabs_and_screenshots_work_as_over_the_port() {
    if !chrome_available() {
        eprintln!("Chrome not found, skipping test");
        return;
    }
    let mut initial_tabs = Vec::new();
    for pipe in [false, true] {
        let browser = Browser::launch_with(|config| config.pipe = pipe)
            .await
            .unwrap();
        assert!(browser.version().await.unwrap().contains("Chrome"));
        let initial = browser.tabs().await.unwrap().len();
        initial_tabs.push(initial);
        let first = browser
            .new_page("data:text/html,<title>first</title><p>one</p>")
            .await
            .unwrap();
        let second = browser
            .new_page("data:text/html,<title>second</title><p>two</p>")
            .await
            .unwrap();
        // Flat sessions: each page's commands reach its own tab.
        assert_eq!(first.title().await.unwrap(), "first");
        assert_eq!(second.title().await.unwrap(), "second");
        assert_eq!(browser.tabs().await.unwrap().len(), initial + 2);
        // A screenshot is one large message.
        let png = first.screenshot().await.unwrap();
        assert!(png.starts_with(b"\x89PNG"));
        let pid = browser.process_id().unwrap();
        browser.close().await.unwrap();
        // SAFETY: signal 0 only checks whether the process exists.
        let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
        assert!(!alive, "close must end Chrome");
    }
    assert_eq!(
        initial_tabs[0], initial_tabs[1],
        "Chrome starts with the same tabs over the pipe and the port"
    );
}
