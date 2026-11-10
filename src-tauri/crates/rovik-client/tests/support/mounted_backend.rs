//! Required Linux FUSE contract, shared by standalone and packaged hosts.
use rovik_client::{
    desktop_api::{
        chat::ChatRequest, overlay::OverlayRequest, projects::ProjectRequest, vfs::VfsRequest,
    },
    desktop_connection::DesktopConnection,
    local_backend::{connect, directory, Lease},
};
use rovik_core::testing::{model, Reply};
use rovik_protocol::{agent::StartRequest, Request, Response};
use serde_json::json;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command},
    time::{Duration, Instant},
};

struct Host(Child);
impl Drop for Host {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        // The test owns this child. Graceful shutdown also tears down its mounts
        // on an assertion failure; a broken shutdown cannot hang the test runner.
        unsafe {
            libc::kill(self.0.id() as i32, libc::SIGTERM);
        }
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            match self.0.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
                _ => {
                    let _ = self.0.kill();
                    let _ = self.0.wait();
                    return;
                }
            }
        }
    }
}

async fn eventually(stage: &str, mut ready: impl AsyncFnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !ready().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("mounted backend timed out: {stage}"));
}

async fn upgrade(endpoint: &Path) -> serde_json::Value {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    async fn frame(stream: &mut tokio::net::UnixStream) -> serde_json::Value {
        let len = stream.read_u32().await.unwrap();
        assert!(len <= 4096);
        let mut bytes = vec![0; len as usize];
        stream.read_exact(&mut bytes).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
    let mut stream = tokio::net::UnixStream::connect(endpoint.join("control.sock"))
        .await
        .unwrap();
    let hello = frame(&mut stream).await;
    let request = serde_json::to_vec(&json!({
        "control": 1, "boot": hello["boot"], "version": env!("CARGO_PKG_VERSION"),
        "protocol": rovik_client::local_backend::PROTOCOL + 1
    }))
    .unwrap();
    stream.write_u32(request.len() as u32).await.unwrap();
    stream.write_all(&request).await.unwrap();
    frame(&mut stream).await
}

async fn refusal(endpoint: &Path, reason: &str) {
    eventually("upgrade refusal after disconnect cleanup", async || {
        let reply = upgrade(endpoint).await;
        let message = reply["Busy"].as_str().unwrap_or_else(|| panic!("{reply}"));
        if message.contains("connected") || message.contains("Blocking") {
            return false;
        }
        assert!(message.contains(reason), "{reply}");
        true
    })
    .await;
}

async fn view(client: &DesktopConnection, chat: &str) -> PathBuf {
    let mut view = None;
    eventually("chat overlay mount", async || {
        let status = client
            .desktop(
                OverlayRequest::Status {
                    dialog_id: chat.into(),
                }
                .into(),
            )
            .await
            .unwrap();
        view = status["view"].as_str().map(PathBuf::from);
        view.is_some()
    })
    .await;
    view.unwrap()
}

async fn tasks(
    client: &DesktopConnection,
    chat: &str,
) -> Vec<rovik_protocol::events::BackgroundTask> {
    match client
        .request(Request::BackgroundTasks {
            dialog_id: chat.into(),
        })
        .await
        .unwrap()
    {
        Response::BackgroundTasks(tasks) => tasks,
        other => panic!("unexpected background task reply: {other:?}"),
    }
}

async fn start(client: &DesktopConnection, chat: &str, project: &str, root: &Path, label: &str) {
    client
        .desktop(
            ChatRequest::CreateDialog {
                id: chat.into(),
                title: label.into(),
                created_at: 1,
                updated_at: 1,
                project_id: Some(project.into()),
            }
            .into(),
        )
        .await
        .unwrap();
    let (url, _) = model(vec![Reply::Call {
        id: "start-server", name: "bash", args: json!({
            "command": format!("printf '{label}' > shared.txt; while :; do if [ -f release ]; then printf '{label}-continued' > continued.txt; fi; sleep 0.02; done"),
            "background": true,
        }),
    },
        // Keep the run alive without invoking automatic end-of-run reviewers.
        Reply::Hang(None),
    ]).await;
    let request: StartRequest = serde_json::from_value(json!({
        "dialogId": chat, "root": root, "isolate": true, "mode": "agent", "contextMode": "trimming",
        "start": {"kind":"send", "message":{"id":format!("message-{chat}"), "content":"Start the fixture server"}},
        "profile": {"id":format!("fixture::{chat}"), "label":"Fixture", "model":"m", "baseUrl":url, "stream":true, "apiRoute":"chat_completions", "contextTokens":100000},
        "config": {"permissions": {"allowAnyCommand":true, "commandApproval":{"required":false,"by":"user"}, "editApproval":{"required":false,"by":"user"}, "toolApproval":{"required":false,"by":"user"}, "verifyChanges":false,"checklist":"off","toolCallCheckpoint":null}}
    })).unwrap();
    client
        .request(Request::AgentStart(Box::new(request)))
        .await
        .unwrap();
}

/// Exercises actual kernel mounts and execution. Missing FUSE is a failure,
/// never a successful capability skip; CI explicitly opts into this contract.
pub async fn check(executable: &Path, flag: Option<&str>) {
    assert_eq!(
        std::env::var("ROVIK_MOUNTED_BACKEND").as_deref(),
        Ok("1"),
        "Run with ROVIK_MOUNTED_BACKEND=1 on a Linux host with working FUSE"
    );
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let config = root.path().join("config");
    let runtime = root.path().join("runtime");
    std::fs::create_dir(&runtime).unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("shared.txt"), "base").unwrap();
    for args in [
        vec!["init", "--quiet"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "Base",
        ],
    ] {
        let result = Command::new("git")
            .arg("-C")
            .arg(&project)
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let mut command = Command::new(executable);
    if let Some(flag) = flag {
        command.arg(flag);
    }
    let mut host = Host(
        command
            .arg("--data")
            .arg(&data)
            .arg("--config")
            .arg(&config)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .spawn()
            .unwrap(),
    );
    let endpoint = directory(&data);
    eventually("host readiness", async || {
        assert!(
            host.0.try_wait().unwrap().is_none(),
            "host exited before readiness"
        );
        connect(&endpoint).await.is_ok()
    })
    .await;
    let attach =
        || DesktopConnection::attach(Command::new("/missing/must-reuse-host"), &data, &config);
    let (a, b) = tokio::join!(attach(), attach());
    let (a, b) = (a.unwrap(), b.unwrap());
    let boot = connect(&endpoint).await.unwrap().1.boot;
    let opened = a
        .desktop(
            ProjectRequest::Open {
                root: project.to_string_lossy().into_owned(),
            }
            .into(),
        )
        .await
        .unwrap();
    // Project ownership remains exclusive to a UI session. The second client
    // views a different folder; explicit chat identities must still select the
    // correct overlays rather than that client's currently selected project.
    assert!(b
        .desktop(
            ProjectRequest::Open {
                root: project.to_string_lossy().into_owned(),
            }
            .into()
        )
        .await
        .unwrap_err()
        .contains("open in another window"));
    let other_project = root.path().join("other-project");
    std::fs::create_dir(&other_project).unwrap();
    std::fs::write(other_project.join("shared.txt"), "other").unwrap();
    b.desktop(
        ProjectRequest::Open {
            root: other_project.to_string_lossy().into_owned(),
        }
        .into(),
    )
    .await
    .unwrap();
    let project_id = opened["project"]["id"].as_str().unwrap();

    let mount = root.path().join("vfs");
    let mounted = a
        .desktop(
            VfsRequest::Mount {
                mountpoint: Some(mount.to_string_lossy().into_owned()),
            }
            .into(),
        )
        .await
        .unwrap();
    assert_eq!(mounted, mount.to_string_lossy().as_ref());
    assert_ne!(
        std::fs::metadata(&mount).unwrap().dev(),
        std::fs::metadata(root.path()).unwrap().dev(),
        "must be an actual kernel mount"
    );
    assert_eq!(
        b.desktop(VfsRequest::Mount { mountpoint: None }.into())
            .await
            .unwrap(),
        mounted
    );
    a.desktop(
        VfsRequest::WriteFile {
            path: "shared.txt".into(),
            content: "virtual".into(),
            encoding: None,
            executable: None,
        }
        .into(),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(mount.join("shared.txt")).unwrap(),
        "virtual"
    );
    assert_eq!(
        b.desktop(
            VfsRequest::ReadFile {
                path: "shared.txt".into()
            }
            .into()
        )
        .await
        .unwrap(),
        "virtual"
    );
    let mut held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(mount.join("shared.txt"))
        .unwrap();

    let chat_a = format!("mounted-a-{}", uuid::Uuid::new_v4());
    let chat_b = format!("mounted-b-{}", uuid::Uuid::new_v4());
    let ((), ()) = tokio::join!(
        start(&a, &chat_a, project_id, &project, "A"),
        start(&b, &chat_b, project_id, &project, "B")
    );
    let (view_a, view_b) = tokio::join!(view(&a, &chat_a), view(&b, &chat_b));
    assert_ne!(view_a, view_b);
    assert_ne!(
        std::fs::metadata(&view_a).unwrap().dev(),
        std::fs::metadata(&view_b).unwrap().dev()
    );
    for chat in [&chat_a, &chat_b] {
        eventually("background tool response", async || {
            let page = b
                .desktop(
                    ChatRequest::GetMessages {
                        dialog_id: chat.clone(),
                        before_seq: None,
                        limit: Some(20),
                    }
                    .into(),
                )
                .await
                .unwrap();
            let messages = page["messages"].as_array().unwrap();
            if let Some(result) = messages.iter().find(|message| message["role"] == "tool") {
                assert!(
                    result["content"]
                        .as_str()
                        .unwrap()
                        .contains("running in the background"),
                    "background command failed: {result}"
                );
                true
            } else {
                false
            }
        })
        .await;
    }
    eventually("isolated command writes", async || {
        std::fs::read_to_string(view_a.join("shared.txt"))
            .ok()
            .as_deref()
            == Some("A")
            && std::fs::read_to_string(view_b.join("shared.txt"))
                .ok()
                .as_deref()
                == Some("B")
    })
    .await;
    eventually("background task registration", async || {
        tasks(&b, &chat_a).await.len() == 1 && tasks(&a, &chat_b).await.len() == 1
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(project.join("shared.txt")).unwrap(),
        "base"
    );
    assert_eq!(
        std::fs::read_to_string(other_project.join("shared.txt")).unwrap(),
        "other"
    );
    a.close().await.unwrap();
    assert_eq!(
        tasks(&b, &chat_a).await.len(),
        1,
        "closing A must not end its backend command"
    );
    b.close().await.unwrap();
    assert!(Lease::acquire(&endpoint).unwrap().is_none());
    std::fs::write(view_a.join("release"), "go").unwrap();
    std::fs::write(view_b.join("release"), "go").unwrap();
    eventually("commands after all clients close", async || {
        std::fs::read_to_string(view_a.join("continued.txt"))
            .ok()
            .as_deref()
            == Some("A-continued")
            && std::fs::read_to_string(view_b.join("continued.txt"))
                .ok()
                .as_deref()
                == Some("B-continued")
    })
    .await;
    held.seek(SeekFrom::End(0)).unwrap();
    held.write_all(b"-retained").unwrap();
    held.seek(SeekFrom::Start(0)).unwrap();
    let mut retained = String::new();
    held.read_to_string(&mut retained).unwrap();
    assert_eq!(retained, "virtual-retained");
    drop(held);

    let c = attach().await.unwrap();
    assert_eq!(connect(&endpoint).await.unwrap().1.boot, boot);
    assert_eq!(
        c.desktop(
            VfsRequest::ReadFile {
                path: "shared.txt".into()
            }
            .into()
        )
        .await
        .unwrap(),
        retained
    );
    // Operations still target chat identity, even from a different client.
    for chat in [&chat_a, &chat_b] {
        c.request(Request::AgentStop {
            dialog_id: chat.clone(),
        })
        .await
        .unwrap();
        for task in tasks(&c, chat).await {
            c.request(Request::BackgroundStop {
                dialog_id: chat.clone(),
                id: task.id,
            })
            .await
            .unwrap();
        }
        if chat == &chat_a {
            assert_eq!(
                tasks(&c, &chat_b).await.len(),
                1,
                "stopping A must preserve B's command"
            );
            std::fs::remove_file(view_b.join("continued.txt")).unwrap();
            eventually("B continues after stopping A", async || {
                std::fs::read_to_string(view_b.join("continued.txt"))
                    .ok()
                    .as_deref()
                    == Some("B-continued")
            })
            .await;
        }
    }
    eventually("agent shutdown", async || {
        match c.request(Request::AgentStatus).await.unwrap() {
            Response::AgentStatus(status) => status.runs.is_empty(),
            other => panic!("{other:?}"),
        }
    })
    .await;
    for chat in [&chat_a, &chat_b] {
        c.desktop(
            OverlayRequest::Discard {
                dialog_id: chat.clone(),
            }
            .into(),
        )
        .await
        .unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(project.join("shared.txt")).unwrap(),
        "base"
    );
    c.close().await.unwrap();
    refusal(&endpoint, "Virtual files").await;
    assert_eq!(
        std::fs::read_to_string(mount.join("shared.txt")).unwrap(),
        retained
    );
    let c = attach().await.unwrap();
    c.desktop(
        VfsRequest::Remove {
            path: "shared.txt".into(),
        }
        .into(),
    )
    .await
    .unwrap();
    // Even an empty volume can still be in use by a shell's working directory
    // or an open directory descriptor. A failed drain must preserve it.
    let directory_handle = std::fs::File::open(&mount).unwrap();
    c.close().await.unwrap();
    refusal(&endpoint, "still in use").await;
    assert_ne!(
        std::fs::metadata(&mount).unwrap().dev(),
        std::fs::metadata(root.path()).unwrap().dev()
    );
    let c = attach().await.unwrap();
    assert_eq!(connect(&endpoint).await.unwrap().1.boot, boot);
    assert_eq!(
        c.desktop(VfsRequest::Mount { mountpoint: None }.into())
            .await
            .unwrap(),
        mounted
    );
    c.desktop(
        VfsRequest::WriteFile {
            path: "after-refusal.txt".into(),
            content: "still-serving".into(),
            encoding: None,
            executable: None,
        }
        .into(),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(mount.join("after-refusal.txt")).unwrap(),
        "still-serving"
    );
    c.desktop(
        VfsRequest::Remove {
            path: "after-refusal.txt".into(),
        }
        .into(),
    )
    .await
    .unwrap();
    c.close().await.unwrap();
    drop(directory_handle);
    eventually("empty unused volume drains for upgrade", async || {
        let reply = upgrade(&endpoint).await;
        if reply == "Retiring" {
            return true;
        }
        assert!(
            reply["Busy"].as_str().is_some_and(
                |message| message.contains("connected") || message.contains("Blocking")
            ),
            "{reply}"
        );
        false
    })
    .await;
    eventually("host shutdown", async || {
        host.0.try_wait().unwrap().is_some_and(|status| {
            assert!(status.success());
            true
        })
    })
    .await;
    assert_eq!(
        std::fs::metadata(&mount).unwrap().dev(),
        std::fs::metadata(root.path()).unwrap().dev()
    );
    assert!(Lease::acquire(&endpoint).unwrap().is_some());
    assert_eq!(
        std::fs::read_to_string(data.join("chats.db.running")).unwrap(),
        "clean"
    );
}
