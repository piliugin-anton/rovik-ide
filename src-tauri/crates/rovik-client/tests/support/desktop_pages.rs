use rovik_client::{
    desktop_api::pages::PagesRequest, desktop_connection::DesktopConnection, desktop_journal::Scope,
};

pub async fn check(one: &DesktopConnection, two: &DesktopConnection) {
    let id = one
        .desktop_stream(
            |sink| {
                let scope = Scope {
                    project_id: Some("page-project".into()),
                    chats: vec!["chat".into()],
                };
                PagesRequest::Subscribe { scope, sink }.into()
            },
            |_| panic!("subscription must not replay page actions"),
        )
        .await
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        two.desktop(PagesRequest::Unsubscribe { id: id.clone() }.into())
            .await
            .unwrap(),
        false
    );
    assert_eq!(
        one.desktop(PagesRequest::Unsubscribe { id: id.clone() }.into())
            .await
            .unwrap(),
        true
    );
    assert_eq!(
        one.desktop(PagesRequest::Unsubscribe { id }.into())
            .await
            .unwrap(),
        false
    );
    assert!(one
        .desktop_stream(
            |sink| PagesRequest::Subscribe {
                scope: Scope {
                    project_id: None,
                    chats: vec!["chat".into(); 257]
                },
                sink
            }
            .into(),
            |_| true
        )
        .await
        .is_err());
    let id = one
        .desktop_stream(
            |sink| {
                PagesRequest::Subscribe {
                    scope: Scope {
                        project_id: None,
                        chats: vec![],
                    },
                    sink,
                }
                .into()
            },
            |_| true,
        )
        .await
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        one.desktop(PagesRequest::Unsubscribe { id }.into())
            .await
            .unwrap(),
        true
    );
}
