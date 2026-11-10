//! Launch the real desktop executable in headless mode, without a display server.
#![cfg(target_os = "linux")]

#[path = "../crates/rovik-client/tests/support/desktop_attach.rs"]
mod desktop_attach;

#[tokio::test]
async fn packaged_desktop_hosts_execution_without_initializing_a_window() {
    desktop_attach::check(
        std::path::Path::new(env!("CARGO_BIN_EXE_rovik-ide")),
        Some(rovik_client::local_backend::host::HOST_FLAG),
    )
    .await;
}

#[path = "../crates/rovik-client/tests/support/mounted_backend.rs"]
mod mounted_backend;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Linux FUSE; run with ROVIK_MOUNTED_BACKEND=1"]
async fn packaged_desktop_preserves_mounted_chat_isolation_without_windows() {
    mounted_backend::check(
        std::path::Path::new(env!("CARGO_BIN_EXE_rovik-ide")),
        Some(rovik_client::local_backend::host::HOST_FLAG),
    )
    .await;
}
