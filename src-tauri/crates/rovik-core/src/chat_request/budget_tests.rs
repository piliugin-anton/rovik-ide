//! Requests against real local sockets: admission surrounds each attempt,
//! and reservations stay live while another child tries to dispatch.

use super::*;
use crate::agent_run::request_budget::{Admission, Ledger};
use crate::testing::{model, Reply};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

fn body() -> Value {
    json!({"model":"budget-test", "messages":[{"role":"user","content":"Actual prepared history"}], "max_tokens":300})
}

fn ledger(limit: u64) -> Arc<Ledger> {
    let ledger = Arc::new(Ledger::default());
    ledger.configure(Some(limit), Some(limit), 0);
    ledger
}

async fn request(
    url: String,
    admission: Option<Admission>,
    body: Value,
    mut cancel: watch::Receiver<bool>,
) -> Result<(), ChatError> {
    let client = reqwest::Client::new();
    run(
        &RequestSpec {
            client: Clients::Only(&client),
            url: &format!("{url}/chat/completions"),
            api_key: "test",
            body: &body,
            streaming: true,
            limits: Limits::default(),
            api: Api::ChatCompletions,
            chat_id: None,
            admission: admission.as_ref(),
        },
        &mut cancel,
        &mut |_| {},
    )
    .await
}

/// Each accepted request waits for its own release, with no timing race.
async fn gated() -> (
    String,
    tokio::sync::mpsc::UnboundedReceiver<(Value, tokio::sync::oneshot::Sender<()>)>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (accepted, received) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let accepted = accepted.clone();
            tokio::spawn(async move {
                let (_, body) = crate::testing::read_request(&mut socket).await;
                let (release, wait) = tokio::sync::oneshot::channel();
                if accepted.send((body, release)).is_err() {
                    return;
                }
                if wait.await.is_err() {
                    return;
                }
                let reply = concat!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                    "data: {\"choices\":[{\"delta\":{\"content\":\"Done\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\n",
                    "data: [DONE]\n\n"
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (url, received)
}

#[tokio::test]
async fn concurrent_child_requests_hold_capacity_and_send_bounded_output() {
    let body = body();
    let input = crate::agent_run::request_budget::prompt_tokens(&body, 3.0);
    let ledger = ledger(2 * (input + 300));
    let (url, mut accepted) = gated().await;
    let (_tx, cancel) = watch::channel(false);
    let first = tokio::spawn(request(
        url.clone(),
        ledger.admission(true),
        body.clone(),
        cancel.clone(),
    ));
    let second = tokio::spawn(request(
        url.clone(),
        ledger.admission(true),
        body.clone(),
        cancel.clone(),
    ));
    let (a, release_a) = tokio::time::timeout(Duration::from_secs(5), accepted.recv())
        .await
        .unwrap()
        .unwrap();
    let (b, release_b) = tokio::time::timeout(Duration::from_secs(5), accepted.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(a["max_tokens"], 300);
    assert_eq!(b["max_tokens"], 300);
    let rejected = request(url, ledger.admission(true), body, cancel)
        .await
        .unwrap_err();
    assert_eq!(rejected.kind, ErrorKind::Budget);
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(ledger.spending(), (30, 30));
}

#[tokio::test]
async fn a_rate_limit_refunds_before_retry_and_reported_usage_settles_the_attempt() {
    let body = body();
    let input = crate::agent_run::request_budget::prompt_tokens(&body, 3.0);
    let ledger = ledger(input + 300);
    let (url, seen) = model(vec![
        Reply::RateLimited(0),
        Reply::Usage(40, 8, Box::new(Reply::Text("Done"))),
    ])
    .await;
    let (_tx, cancel) = watch::channel(false);
    request(url, ledger.admission(true), body, cancel)
        .await
        .unwrap();
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert_eq!(ledger.spending(), (48, 48));
}

#[tokio::test]
async fn an_uncertain_failed_attempt_cannot_retry_with_the_same_allowance() {
    let body = body();
    let input = crate::agent_run::request_budget::prompt_tokens(&body, 3.0);
    let ledger = ledger(input + 300);
    let (url, seen) = model(vec![
        Reply::Status(500, r#"{"error":{"message":"uncertain failure"}}"#),
        Reply::Text("Would overspend"),
    ])
    .await;
    let (_tx, cancel) = watch::channel(false);
    let error = request(url, ledger.admission(true), body, cancel)
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Budget);
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(ledger.spending(), (input + 300, input + 300));
}

#[tokio::test]
async fn a_success_without_usage_charges_its_estimate_and_partial_usage_keeps_the_unknown_part() {
    let body = body();
    let input = crate::agent_run::request_budget::prompt_tokens(&body, 3.0);
    let ledger = ledger(5_000);
    let (url, _) = model(vec![
        Reply::Text("No usage"),
        Reply::UsageAs(
            json!({"prompt_tokens":40}),
            Box::new(Reply::Text("Only input usage")),
        ),
    ])
    .await;
    let (_tx, cancel) = watch::channel(false);
    request(
        url.clone(),
        ledger.admission(true),
        body.clone(),
        cancel.clone(),
    )
    .await
    .unwrap();
    assert_eq!(ledger.spending().0, input + 300);
    request(url, ledger.admission(true), body, cancel)
        .await
        .unwrap();
    assert_eq!(ledger.spending().0, input + 300 + 40 + 300);
}

#[tokio::test]
async fn cancellation_before_dispatch_is_free_but_dropping_a_dispatched_future_is_charged() {
    let body = body();
    let input = crate::agent_run::request_budget::prompt_tokens(&body, 3.0);
    let ledger = ledger(input + 300);
    let (url, mut accepted) = gated().await;
    let (_stopped, cancel) = watch::channel(true);
    assert_eq!(
        request(url.clone(), ledger.admission(true), body.clone(), cancel)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Cancelled
    );
    assert_eq!(ledger.spending().0, 0);
    assert!(accepted.try_recv().is_err());
    let (_tx, cancel) = watch::channel(false);
    let active = tokio::spawn(request(url, ledger.admission(true), body, cancel));
    let (_, release) = tokio::time::timeout(Duration::from_secs(5), accepted.recv())
        .await
        .unwrap()
        .unwrap();
    active.abort();
    assert!(active.await.unwrap_err().is_cancelled());
    drop(release);
    assert_eq!(ledger.spending().0, input + 300);
    // A fresh root can spend its own allowance even after this root stopped.
    let independent = self::ledger(input + 300);
    assert!(independent
        .admission(true)
        .unwrap()
        .reserve(input, 300)
        .is_ok());
}

#[tokio::test]
async fn no_token_limit_keeps_the_original_output_options() {
    let mut body = body();
    body.as_object_mut().unwrap().remove("max_tokens");
    let (url, seen) = model(vec![Reply::Text("Done")]).await;
    let (_tx, cancel) = watch::channel(false);
    request(url, None, body.clone(), cancel).await.unwrap();
    assert_eq!(seen.lock().unwrap().as_slice(), &[body]);
}

#[tokio::test]
async fn a_provider_requiring_max_completion_tokens_keeps_the_cap_on_fallback() {
    let body = body();
    let input = crate::agent_run::request_budget::prompt_tokens(&body, 3.0);
    let ledger = ledger(input + 300);
    let (url, seen) = model(vec![
        Reply::Status(400, r#"{"error":{"message":"Unsupported parameter max_tokens; use max_completion_tokens instead"}}"#),
        Reply::Usage(40, 8, Box::new(Reply::Text("Done"))),
    ]).await;
    let client = reqwest::Client::new();
    let (_tx, mut cancel) = watch::channel(false);
    let admission = ledger.admission(true).unwrap();
    let (result, _) = crate::agent::run_with_reasoning_admitted(
        Clients::Only(&client),
        &format!("{url}/chat/completions"),
        "test",
        &body,
        true,
        Limits::default(),
        None,
        Vec::new(),
        Some(Api::ChatCompletions),
        None,
        Some(&admission),
        &mut cancel,
        &mut |_| {},
    )
    .await;
    result.unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0]["max_tokens"], 300);
    assert_eq!(seen[1]["max_completion_tokens"], 300);
    assert!(seen[1].get("max_tokens").is_none());
    assert_eq!(ledger.spending().0, 48);
}

#[tokio::test]
async fn cancellation_keeps_already_reported_input_and_charges_unknown_output() {
    let body = body();
    let ledger = ledger(5_000);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (sent, mut read) = watch::channel(false);
    let (_keep_alive, hold) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        crate::testing::read_request(&mut socket).await;
        socket.write_all(concat!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"started\"}}],\"usage\":{\"prompt_tokens\":40}}\n\n"
        ).as_bytes()).await.unwrap();
        let _ = hold.await;
    });
    let client = reqwest::Client::new();
    let admission = ledger.admission(true).unwrap();
    let (cancel_tx, mut cancel) = watch::channel(false);
    let stop = tokio::spawn(async move {
        read.changed().await.unwrap();
        cancel_tx.send(true).unwrap();
    });
    let error = run(
        &RequestSpec {
            client: Clients::Only(&client),
            url: &format!("{url}/chat/completions"),
            api_key: "test",
            body: &body,
            streaming: true,
            limits: Limits::default(),
            api: Api::ChatCompletions,
            chat_id: None,
            admission: Some(&admission),
        },
        &mut cancel,
        &mut |event| {
            if matches!(event, crate::agent::ChatStreamEvent::Delta { .. }) {
                let _ = sent.send(true);
            }
        },
    )
    .await
    .unwrap_err();
    stop.await.unwrap();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(ledger.spending(), (340, 340));
}
