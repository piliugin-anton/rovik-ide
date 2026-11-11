use rovik_client::{desktop_api::workload::WorkloadRequest, desktop_connection::DesktopConnection};
use serde_json::Value;
async fn focus(client: &DesktopConnection, dialog: Option<&str>) {
    assert!(client
        .desktop(
            WorkloadRequest::Focus {
                dialog_id: dialog.map(str::to_string)
            }
            .into()
        )
        .await
        .unwrap()
        .is_null());
}
async fn status(client: &DesktopConnection) -> Value {
    client
        .desktop(WorkloadRequest::Status.into())
        .await
        .unwrap()
}
/// Both transports share queue settings while retaining each client's selection.
pub async fn check(a: DesktopConnection, b: DesktopConnection) {
    let saved = status(&a).await["slots"].as_u64().map(|n| n as usize);
    a.desktop(WorkloadRequest::SetSlots { slots: Some(999) }.into())
        .await
        .unwrap();
    assert_eq!(status(&b).await["slots"], 8);
    b.desktop(WorkloadRequest::SetSlots { slots: Some(0) }.into())
        .await
        .unwrap();
    assert_eq!(status(&a).await["slots"], 1);
    b.desktop(WorkloadRequest::SetSlots { slots: None }.into())
        .await
        .unwrap();
    assert!(status(&a).await["slots"].is_null());
    focus(&a, Some("chat-a")).await;
    focus(&b, Some("chat-b")).await;
    focus(&a, None).await;
    assert_eq!(status(&b).await["focusedDialogId"], "chat-b");
    focus(&a, Some("chat-a")).await;
    assert_eq!(status(&b).await["focusedDialogId"], "chat-a");
    drop(a);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while status(&b).await["focusedDialogId"] != "chat-b" {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    focus(&b, None).await;
    assert!(status(&b).await["focusedDialogId"].is_null());
    b.desktop(WorkloadRequest::SetSlots { slots: saved }.into())
        .await
        .unwrap();
}
