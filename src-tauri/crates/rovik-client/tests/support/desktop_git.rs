use rovik_client::{desktop_api::git::GitRequest, desktop_connection::DesktopConnection};

/// Both transports execute Git against the requested tree, preserving UI replies.
pub async fn check(a: &DesktopConnection, b: &DesktopConnection, owners: &[u32]) {
    let one = tempfile::tempdir().unwrap();
    let two = tempfile::tempdir().unwrap();
    let path = one.path().to_string_lossy().into_owned();
    let other = two.path().to_string_lossy().into_owned();
    for (client, root) in [(a, &path), (b, &other)] {
        client
            .desktop(GitRequest::Init { path: root.clone() }.into())
            .await
            .unwrap();
        let repo = git2::Repository::open(root).unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Desktop test").unwrap();
        config
            .set_str("user.email", "desktop@example.test")
            .unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let hook = one.path().join(".git/hooks/pre-commit");
        std::fs::write(
            &hook,
            "#!/bin/sh\nprintf '%s' \"$PPID\" > .git/hook-parent\n",
        )
        .unwrap();
        std::fs::set_permissions(hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = owners;
    std::fs::write(one.path().join("file.txt"), "original\n").unwrap();
    a.desktop(
        GitRequest::Commit {
            path: path.clone(),
            message: "first".into(),
            no_verify: None,
        }
        .into(),
    )
    .await
    .unwrap();
    #[cfg(unix)]
    {
        let parent: u32 = std::fs::read_to_string(one.path().join(".git/hook-parent"))
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            owners.contains(&parent),
            "Git hook ran in unexpected process {parent}"
        );
    }
    let status = b
        .desktop(GitRequest::Status { path: path.clone() }.into())
        .await
        .unwrap();
    assert_eq!(status["files"].as_array().unwrap().len(), 0);
    let other_status = b
        .desktop(
            GitRequest::Status {
                path: other.clone(),
            }
            .into(),
        )
        .await
        .unwrap();
    assert_eq!(other_status["branch"], "unborn");
    std::fs::write(one.path().join("file.txt"), "changed\n").unwrap();
    let status = a
        .desktop(GitRequest::Status { path: path.clone() }.into())
        .await
        .unwrap();
    assert_eq!(status["files"][0]["path"], "file.txt");
    let original = b
        .desktop(
            GitRequest::BaseContent {
                path: path.clone(),
                file: "file.txt".into(),
            }
            .into(),
        )
        .await
        .unwrap();
    assert_eq!(original, "original\n");
    a.desktop(
        GitRequest::StashPush {
            path: path.clone(),
            message: Some("saved".into()),
        }
        .into(),
    )
    .await
    .unwrap();
    let stashes = b
        .desktop(GitRequest::StashList { path: path.clone() }.into())
        .await
        .unwrap();
    assert_eq!(stashes.as_array().unwrap().len(), 1);
    b.desktop(
        GitRequest::StashPop {
            path: path.clone(),
            index: Some(0),
        }
        .into(),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(one.path().join("file.txt")).unwrap(),
        "changed\n"
    );
    let log = || {
        GitRequest::Log {
            path: path.clone(),
            limit: Some(10),
            query: None,
            offset: None,
            tip: None,
        }
        .into()
    };
    let (first, second) = tokio::join!(a.desktop(log()), b.desktop(log()));
    for result in [first, second] {
        assert_eq!(result.unwrap()["commits"].as_array().unwrap().len(), 1);
    }
    let error = b
        .desktop(
            GitRequest::BaseContent {
                path: path.clone(),
                file: "../outside".into(),
            }
            .into(),
        )
        .await
        .unwrap_err();
    assert!(error.contains("should not start with `..`"), "{error}");
    // An application-level error keeps the connection usable.
    b.desktop(
        GitRequest::CreateBranch {
            path: path.clone(),
            name: "from-second-window".into(),
            checkout: false,
            from: None,
        }
        .into(),
    )
    .await
    .unwrap();
    let branches = a
        .desktop(GitRequest::Branches { path: path.clone() }.into())
        .await
        .unwrap();
    assert!(branches.to_string().contains("from-second-window"));
    assert!(git2::Repository::open(&other).unwrap().head().is_err());
}
