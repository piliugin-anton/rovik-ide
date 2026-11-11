#![cfg(unix)]
use rovik_client::{
    desktop_api::formatter::FormatterRequest, desktop_connection::DesktopConnection,
};
use serde_json::Value;
use std::{os::unix::fs::PermissionsExt, path::Path, time::Duration};

struct Project(tempfile::TempDir);
impl Project {
    fn new(script: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("node_modules/.bin")).unwrap();
        std::fs::write(root.path().join(".prettierrc"), "{}").unwrap();
        std::fs::write(root.path().join("file.ts"), "on disk").unwrap();
        let tool = root.path().join("node_modules/.bin/prettier");
        std::fs::write(&tool, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self(root)
    }
    fn request(&self, sink: String, content: &str) -> FormatterRequest {
        FormatterRequest::Format {
            path: self.0.path().join("file.ts").to_string_lossy().into_owned(),
            language: "typescript".into(),
            root: Some(self.0.path().to_string_lossy().into_owned()),
            content: content.into(),
            sink,
        }
    }
    async fn format(&self, client: &DesktopConnection, content: &str) -> Result<Value, Value> {
        client
            .desktop_task(|sink| self.request(sink, content).into(), |_| Ok(()))
            .await
    }
}
async fn until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("formatter fixture reached expected state");
}
fn gone(pid: i32) -> bool {
    // Linux may retain an orphan zombie until the container's init reaps it.
    if std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
        s.rsplit_once(") ")
            .is_some_and(|(_, rest)| rest.starts_with('Z'))
    }) {
        return true;
    }
    unsafe { libc::kill(pid, 0) != 0 }
}
fn pid(path: &Path) -> Option<i32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Both transports preserve buffer results, backend execution, and client cleanup.
pub async fn check(a: DesktopConnection, b: DesktopConnection, owners: &[u32]) {
    let first = Project::new("printf '%s:' \"$PPID\"; tr a-z A-Z");
    let second = Project::new("tr A-Z a-z");
    let lookup = a
        .desktop_task(
            |sink| {
                FormatterRequest::Lookup {
                    path: first
                        .0
                        .path()
                        .join("file.ts")
                        .to_string_lossy()
                        .into_owned(),
                    language: "typescript".into(),
                    root: Some(first.0.path().to_string_lossy().into_owned()),
                    sink,
                }
                .into()
            },
            |_| Ok(()),
        )
        .await
        .unwrap();
    assert_eq!(lookup["formatter"]["id"], "prettier");
    assert_eq!(lookup["formatter"]["declared"], true);
    let out = first.format(&a, "unsaved text\n").await.unwrap();
    let (parent, text) = out.as_str().unwrap().split_once(':').unwrap();
    assert!(owners.contains(&parent.parse().unwrap()));
    assert_eq!(text, "UNSAVED TEXT\n");
    assert_eq!(
        std::fs::read_to_string(first.0.path().join("file.ts")).unwrap(),
        "on disk"
    );
    assert_eq!(second.format(&b, "SECOND\n").await.unwrap(), "second\n");
    let broken = Project::new("echo 'invalid formatter config' >&2; exit 2");
    assert!(broken
        .format(&a, "x")
        .await
        .unwrap_err()
        .as_str()
        .unwrap()
        .contains("invalid formatter config"));
    assert_eq!(
        second.format(&a, "AFTER ERROR").await.unwrap(),
        "after error"
    );
    // A blocked stdin writer cannot hold its RPC connection or another window.
    let hanging = Project::new("echo $$ > parent; sleep 1000 & echo $! > child; wait");
    a.desktop_stream(
        |sink| hanging.request(sink, &"x".repeat(2 * 1024 * 1024)).into(),
        |_| true,
    )
    .await
    .unwrap();
    let parent = hanging.0.path().join("parent");
    let child = hanging.0.path().join("child");
    until(|| pid(&parent).is_some() && pid(&child).is_some()).await;
    let (parent, child) = (pid(&parent).unwrap(), pid(&child).unwrap());
    assert_eq!(
        second.format(&a, "SAME CONNECTION").await.unwrap(),
        "same connection"
    );
    assert_eq!(
        second.format(&b, "OTHER WINDOW").await.unwrap(),
        "other window"
    );
    drop(a);
    until(|| gone(parent) && gone(child)).await;
    assert_eq!(
        second.format(&b, "STILL CONNECTED").await.unwrap(),
        "still connected"
    );
}
