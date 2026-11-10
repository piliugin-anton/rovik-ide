#![cfg(unix)]
use rovik_client::{desktop_api::lsp::LspRequest, desktop_connection::DesktopConnection};
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

// A local fixture, not a dependency on an installed language server. Its child
// deliberately inherits stdout so cleanup must end the process tree to drain it.
const SERVER: &str = r#"
export LC_ALL=C
emit() { printf 'Content-Length: %s\r\n\r\n%s' "${#1}" "$1"; }
sleep 1000 &
job=$!
emit "{\"pid\":$$,\"parent\":$PPID,\"child\":$job}"
printf 'server ready\n' >&2
if [ "$1" = stall ]; then wait; exit; fi
while IFS= read -r header; do
  count=$(printf '%s' "$header" | tr -cd '0-9')
  IFS= read -r blank
  body=$(dd bs=1 count="$count" 2>/dev/null)
  emit "$body"
  if [ "$body" = exit ]; then exit 0; fi
done
"#;

async fn event(events: &mut UnboundedReceiver<Value>, kind: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = events.recv().await.expect("event stream closed");
            if event["type"] == kind {
                return event;
            }
        }
    })
    .await
    .expect("language server event")
}

async fn start(
    client: &DesktopConnection,
    mode: &str,
) -> (String, UnboundedReceiver<Value>, Value) {
    let (sent, mut events) = unbounded_channel();
    let id = client
        .desktop_stream(
            |sink| {
                LspRequest::Start {
                    command: "sh".into(),
                    args: vec!["-c".into(), SERVER.into(), "fixture".into(), mode.into()],
                    cwd: "/tmp".into(),
                    sink,
                }
                .into()
            },
            move |event| sent.send(event).is_ok(),
        )
        .await
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let ready = event(&mut events, "message").await;
    let process: Value = serde_json::from_str(ready["data"].as_str().unwrap()).unwrap();
    (id, events, process)
}

async fn send(client: &DesktopConnection, id: &str, message: &str) -> Result<Value, Value> {
    client
        .desktop_task(
            |sink| {
                LspRequest::Send {
                    id: id.into(),
                    message: message.into(),
                    sink,
                }
                .into()
            },
            |_| Ok(()),
        )
        .await
}

async fn ended(pid: u32) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            // A grandchild can remain a zombie until PID 1 reaps it; it is no
            // longer running and holds no pipes. Linux exposes that state.
            #[cfg(target_os = "linux")]
            if std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
                s.rsplit_once(") ")
                    .is_some_and(|(_, rest)| rest.starts_with('Z'))
            }) {
                return;
            }
            if unsafe { libc::kill(pid as i32, 0) } != 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("server process ended");
}

/// Private ownership, framing, process execution, and cleanup work over either transport.
pub async fn check(a: DesktopConnection, b: DesktopConnection, owners: &[u32]) {
    let (a_id, mut a_events, a_process) = start(&a, "echo").await;
    let (b_id, mut b_events, _) = start(&b, "echo").await;
    assert!(owners.contains(&(a_process["parent"].as_u64().unwrap() as u32)));
    assert!(send(&b, &a_id, "wrong window")
        .await
        .unwrap_err()
        .as_str()
        .unwrap()
        .contains("not open in this client"));
    b.desktop(LspRequest::Stop { id: a_id.clone() }.into())
        .await
        .unwrap();
    send(&a, &a_id, "{\"message\":\"ж\"}").await.unwrap();
    assert_eq!(
        event(&mut a_events, "message").await["data"],
        "{\"message\":\"ж\"}"
    );
    a.close().await.unwrap();
    assert!(send(&a, &a_id, "closed").await.is_err());
    ended(a_process["pid"].as_u64().unwrap() as u32).await;
    ended(a_process["child"].as_u64().unwrap() as u32).await;
    send(&b, &b_id, "still alive").await.unwrap();
    assert_eq!(event(&mut b_events, "message").await["data"], "still alive");
    send(&b, &b_id, "exit").await.unwrap();
    assert_eq!(event(&mut b_events, "message").await["data"], "exit");
    assert_eq!(event(&mut b_events, "exit").await["code"], 0);

    let (stalled, _stalled_events, process) = start(&b, "stall").await;
    let (sent, mut completion) = unbounded_channel();
    tokio::time::timeout(
        Duration::from_secs(5),
        b.desktop_stream(
            |sink| {
                LspRequest::Send {
                    id: stalled.clone(),
                    message: "x".repeat(1024 * 1024),
                    sink,
                }
                .into()
            },
            move |value| sent.send(value).is_ok(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        b.desktop(LspRequest::Stop { id: stalled }.into()),
    )
    .await
    .unwrap()
    .unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(10), completion.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(completed["value"]["Err"].is_string(), "{completed}");
    ended(process["pid"].as_u64().unwrap() as u32).await;
    ended(process["child"].as_u64().unwrap() as u32).await;

    // Losing only a callback must end that producer, not the client's next server.
    let (sent, mut rejected) = unbounded_channel();
    b.desktop_stream(
        |sink| {
            LspRequest::Start {
                command: "sh".into(),
                args: vec!["-c".into(), SERVER.into(), "fixture".into(), "echo".into()],
                cwd: "/tmp".into(),
                sink,
            }
            .into()
        },
        move |value| {
            if value["type"] == "message" {
                let _ = sent.send(value);
                false
            } else {
                true
            }
        },
    )
    .await
    .unwrap();
    let rejected = tokio::time::timeout(Duration::from_secs(10), rejected.recv())
        .await
        .unwrap()
        .unwrap();
    let process: Value = serde_json::from_str(rejected["data"].as_str().unwrap()).unwrap();
    ended(process["pid"].as_u64().unwrap() as u32).await;
    ended(process["child"].as_u64().unwrap() as u32).await;
    let error = b
        .desktop_task(
            |sink| {
                LspRequest::Find {
                    custom_path: Some("/no/such/rust-analyzer".into()),
                    prefer_system: false,
                    sink,
                }
                .into()
            },
            |_| Ok(()),
        )
        .await
        .unwrap_err();
    assert!(error
        .as_str()
        .unwrap()
        .contains("doesn't run as rust-analyzer"));
    let (id, mut events, _) = start(&b, "echo").await;
    send(&b, &id, "after callback loss").await.unwrap();
    assert_eq!(
        event(&mut events, "message").await["data"],
        "after callback loss"
    );
    b.desktop(LspRequest::Stop { id }.into()).await.unwrap();
}
