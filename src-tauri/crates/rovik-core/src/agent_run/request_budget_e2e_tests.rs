//! Full harness dispatch: children reserve from one root while their
//! HTTP replies are still blocked at the provider.
use super::*;
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn parallel_children_reserve_the_run_budget_before_sending_requests() {
    let h = harness(&[("a.txt", "x\n")]).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (accepted, mut requests) = tokio::sync::mpsc::unbounded_channel();
    let lead_turns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let (accepted, turns) = (accepted.clone(), lead_turns.clone());
            tokio::spawn(async move {
                let (path, body) = crate::testing::read_request(&mut socket).await;
                if path.ends_with("/responses") {
                    let _ = socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                    return;
                }
                let child = route_of(&body, &["BUDGET-A", "BUDGET-B", "BUDGET-C"]).is_some();
                let delta = if child {
                    let (release, wait) = tokio::sync::oneshot::channel();
                    accepted.send((body, release)).unwrap();
                    if wait.await.is_err() {
                        return;
                    }
                    json!({"content": REPORT})
                } else if turns.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    json!({"tool_calls": [
                        {"index":0,"id":"budget-a","type":"function","function":{"name":"task","arguments":json!({"kind":"scout","goal":"Find BUDGET-A"}).to_string()}},
                        {"index":1,"id":"budget-b","type":"function","function":{"name":"task","arguments":json!({"kind":"scout","goal":"Find BUDGET-B"}).to_string()}},
                        {"index":2,"id":"budget-c","type":"function","function":{"name":"task","arguments":json!({"kind":"scout","goal":"Find BUDGET-C"}).to_string()}}
                    ]})
                } else {
                    json!({"content":"Done."})
                };
                let reply = json!({"choices":[{"delta":delta,"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":10}});
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {reply}\n\ndata: [DONE]\n\n");
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    let mut config = scouts_on(json!({}));
    config.kinds.get_mut("scout").unwrap().options = Some(crate::agent::OpenAIChatOptions {
        max_tokens: Some(100_000),
        ..Default::default()
    });
    config.max_run_tokens = Some(200_000);
    let mut request = h.with_subagents(&url, "Survey the project", quiet_permissions(), config);
    request.profile.api_route = Some("chat_completions".into());
    h.start(request).await;
    let first = tokio::time::timeout(Duration::from_secs(10), requests.recv())
        .await
        .unwrap()
        .unwrap();
    let second = tokio::time::timeout(Duration::from_secs(10), requests.recv())
        .await
        .unwrap()
        .unwrap();
    // Neither provider request can finish until the third child declines.
    h.wait_for("a child declined admission", |event| {
        event["type"] == "subagentEnded" && event["status"] == "partial"
    })
    .await;
    let admitted: u64 = [&first.0, &second.0]
        .iter()
        .map(|body| {
            crate::agent_run::request_budget::prompt_tokens(
                body,
                crate::agent::calibration::chars_per_token(&crate::agent::calibration::key(
                    &url,
                    body["model"].as_str().unwrap(),
                )),
            ) + body["max_tokens"].as_u64().unwrap()
        })
        .sum();
    assert!(admitted <= 200_000, "reserved {admitted}");
    assert!(first.0["max_tokens"].as_u64().unwrap() <= 100_000);
    assert!(second.0["max_tokens"].as_u64().unwrap() <= 100_000);
    assert!(
        requests.try_recv().is_err(),
        "the refused child reached the provider"
    );
    first.1.send(()).unwrap();
    second.1.send(()).unwrap();
    assert_eq!(h.run_end().await, "done");
    let ended = h.events_of("subagentEnded");
    assert_eq!(ended.iter().filter(|e| e["status"] == "done").count(), 2);
    assert_eq!(ended.iter().filter(|e| e["status"] == "partial").count(), 1);
}
