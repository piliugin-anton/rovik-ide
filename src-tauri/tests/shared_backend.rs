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
