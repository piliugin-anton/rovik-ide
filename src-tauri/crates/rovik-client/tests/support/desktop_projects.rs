use rovik_client::{
    desktop_api::{
        chat::{Attachment, ChatRequest},
        projects::ProjectRequest,
    },
    desktop_connection::DesktopConnection,
};
use std::time::Duration;

/// Shared ownership guards and lease cleanup must work identically across IPC.
pub async fn check(a: &DesktopConnection, b: DesktopConnection) {
    let folders = tempfile::tempdir().unwrap();
    let one = folders.path().join("one");
    let two = folders.path().join("two");
    std::fs::create_dir(&one).unwrap();
    std::fs::create_dir(&two).unwrap();
    let one = one.to_string_lossy().into_owned();
    let two = two.to_string_lossy().into_owned();
    let opened_a = a
        .desktop(ProjectRequest::Open { root: one.clone() }.into())
        .await
        .unwrap();
    let opened_b = b
        .desktop(ProjectRequest::Open { root: two.clone() }.into())
        .await
        .unwrap();
    let id_a = opened_a["project"]["id"].as_str().unwrap().to_string();
    let id_b = opened_b["project"]["id"].as_str().unwrap().to_string();
    assert_ne!(id_a, id_b);
    for request in [
        ProjectRequest::Open { root: one.clone() },
        ProjectRequest::Delete {
            project_id: id_a.clone(),
        },
        ProjectRequest::AnswerCopy {
            root: two.clone(),
            original_id: id_a.clone(),
            share: true,
        },
        ProjectRequest::AnswerMoved {
            root: two.clone(),
            current_id: id_b.clone(),
            target_id: id_a.clone(),
        },
        ProjectRequest::Relink {
            project_id: id_b.clone(),
            root: one.clone(),
            merge: true,
        },
    ] {
        let error = b.desktop(request.into()).await.unwrap_err();
        assert!(error.contains("open in another window"), "{error}");
    }
    // Switching this window must release only its own registration.
    b.desktop(ProjectRequest::Clear.into()).await.unwrap();
    assert!(b
        .desktop(ProjectRequest::Open { root: one.clone() }.into())
        .await
        .unwrap_err()
        .contains("open in another window"));
    b.desktop(ProjectRequest::Open { root: two.clone() }.into())
        .await
        .unwrap();
    drop(b);
    // The daemon processes EOF asynchronously; synchronize on the actual lease release.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match a
                .desktop(
                    ProjectRequest::Delete {
                        project_id: id_b.clone(),
                    }
                    .into(),
                )
                .await
            {
                Ok(_) => break,
                Err(e) => {
                    assert!(e.contains("open in another window"), "{e}");
                    tokio::task::yield_now().await;
                }
            }
        }
    })
    .await
    .unwrap();
    a.desktop(ProjectRequest::Clear.into()).await.unwrap();
    let moved = folders.path().join("moved");
    std::fs::create_dir(&moved).unwrap();
    let moved = moved.to_string_lossy().into_owned();
    let relinked = a
        .desktop(
            ProjectRequest::Relink {
                project_id: id_a.clone(),
                root: moved.clone(),
                merge: false,
            }
            .into(),
        )
        .await
        .unwrap();
    assert_eq!(relinked["kind"], "done");
    let opened = a
        .desktop(
            ProjectRequest::Open {
                root: moved.clone(),
            }
            .into(),
        )
        .await
        .unwrap();
    assert_eq!(opened["project"]["id"], id_a);
    let copied = a
        .desktop(
            ProjectRequest::AnswerCopy {
                root: moved.clone(),
                original_id: id_a.clone(),
                share: false,
            }
            .into(),
        )
        .await
        .unwrap();
    let separate = copied["id"].as_str().unwrap().to_string();
    assert_ne!(separate, id_a);
    let merged = a
        .desktop(
            ProjectRequest::AnswerMoved {
                root: moved,
                current_id: separate,
                target_id: id_a.clone(),
            }
            .into(),
        )
        .await
        .unwrap();
    assert_eq!(merged["id"], id_a);
    a.desktop(ProjectRequest::Delete { project_id: id_a }.into())
        .await
        .unwrap();

    let pdf = b"%PDF-1.4\n1 0 obj << /Type /Page >> endobj\n%%EOF".to_vec();
    let stored = a
        .desktop(ChatRequest::StoreAttachment { bytes: pdf.clone() }.into())
        .await
        .unwrap();
    let value = a
        .desktop(
            ChatRequest::ReadAttachment {
                path: format!("/{}", stored["id"].as_str().unwrap()),
            }
            .into(),
        )
        .await
        .unwrap();
    let attachment: Attachment = serde_json::from_value(value).unwrap();
    assert_eq!(attachment.mime, "application/pdf");
    assert_eq!(attachment.bytes, pdf);
    assert!(a
        .desktop(
            ChatRequest::ReadAttachment {
                path: "../outside".into()
            }
            .into()
        )
        .await
        .unwrap()
        .is_null());
}
