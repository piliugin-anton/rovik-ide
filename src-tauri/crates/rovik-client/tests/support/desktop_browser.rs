use rovik_client::{desktop_api::browser::BrowserRequest, desktop_connection::DesktopConnection};

/// Read-only status and validation failures retain their contracts on both transports.
/// These checks deliberately need neither a downloaded browser nor public network access.
pub async fn check(a: &DesktopConnection, b: &DesktopConnection) {
    let status = a.desktop(BrowserRequest::Status.into()).await.unwrap();
    assert_eq!(status["name"], rovik_core::pinned_browser::NAME);
    assert_eq!(status["version"], rovik_core::pinned_browser::VERSION);
    assert!(status["path"].is_null() || status["path"].is_string());
    for screenshot in [false, true] {
        let error = a
            .desktop_task(
                |sink| {
                    if screenshot {
                        BrowserRequest::Screenshot {
                            url: Some("file:///etc/passwd".into()),
                            sink,
                        }
                    } else {
                        BrowserRequest::Browse {
                            url: "file:///etc/passwd".into(),
                            max_chars: None,
                            sink,
                        }
                    }
                    .into()
                },
                |_| Ok(()),
            )
            .await
            .unwrap_err();
        assert!(
            error.as_str().unwrap().contains("Only http(s) URLs"),
            "{error}"
        );
    }
    let error = b
        .desktop_task(
            |sink| {
                BrowserRequest::Search {
                    query: "  ".into(),
                    max_results: None,
                    sink,
                }
                .into()
            },
            |_| Ok(()),
        )
        .await
        .unwrap_err();
    assert_eq!(error, "Search query is empty.");
    let error = b
        .desktop_task(
            |sink| {
                BrowserRequest::Browse {
                    url: "http://169.254.169.254/".into(),
                    max_chars: None,
                    sink,
                }
                .into()
            },
            |_| Ok(()),
        )
        .await
        .unwrap_err();
    assert!(
        error.as_str().unwrap().contains("private network"),
        "{error}"
    );
    // Validation errors neither break the session nor alter shared installation status.
    assert_eq!(
        b.desktop(BrowserRequest::Status.into()).await.unwrap(),
        status
    );
}
