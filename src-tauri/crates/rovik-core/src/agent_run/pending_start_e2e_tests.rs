use super::*;

#[tokio::test]
async fn stop_cancels_an_accepted_start_before_controls_exist_without_waiting_for_its_driver() {
    let h = harness(&[]).await;
    let driving = autopilot::drive_lock(&h.dialog_id).await;
    let url = format!("http://127.0.0.1:{NOTHING_LISTENS}");
    let request = h.request(&url, Harness::send("Hello"), quiet_permissions());
    let mut starting = Box::pin(agent_start(&h.core, request));
    assert!(futures_util::poll!(starting.as_mut()).is_pending());
    assert!(!h.core.agents.is_running(&h.dialog_id));

    assert!(tokio::time::timeout(
        Duration::from_secs(2),
        agent_stop(&h.core, h.dialog_id.clone())
    )
    .await
    .unwrap());
    let error = tokio::time::timeout(Duration::from_secs(2), starting)
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.contains("start was stopped"), "{error}");
    assert!(!h.core.agents.is_running(&h.dialog_id));
    drop(driving);

    let dialog_id = h.dialog_id.clone();
    let history = h
        .core
        .db
        .read(move |db| store::get_messages(db, &dialog_id, None, 10))
        .await
        .unwrap();
    assert!(
        history.messages.is_empty(),
        "the canceled start stored no message"
    );
    assert!(h.core.agents.begin_start(&h.dialog_id).check().is_ok());
}

#[tokio::test]
async fn stop_cancels_a_start_waiting_for_a_file_change_before_the_files_are_released() {
    let h = harness(&[]).await;
    let held = h.core.agents.hold_files(&h.dialog_id).unwrap();
    let url = format!("http://127.0.0.1:{NOTHING_LISTENS}");
    let request = h.request(&url, Harness::send("Hello"), quiet_permissions());
    let mut starting = Box::pin(super::super::start_run(&h.core, request));
    tokio::time::timeout(
        Duration::from_secs(2),
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(starting.as_mut(), cx).is_pending());
            if h.core.agents.lock().files_held[&h.dialog_id].receiver_count() > 0 {
                std::task::Poll::Ready(())
            } else {
                std::task::Poll::Pending
            }
        }),
    )
    .await
    .unwrap();
    assert!(!h.core.agents.is_running(&h.dialog_id));
    assert!(agent_stop(&h.core, h.dialog_id.clone()).await);
    let error = tokio::time::timeout(Duration::from_secs(2), starting)
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.contains("start was stopped"), "{error}");
    assert!(!h.core.agents.is_running(&h.dialog_id));
    drop(held);
}
