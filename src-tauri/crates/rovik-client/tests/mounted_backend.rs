//! Required mounted shared-backend contract; CI runs it explicitly on Linux.
#![cfg(target_os = "linux")]
#[path = "support/mounted_backend.rs"]
mod mounted_backend;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Linux FUSE; run with ROVIK_MOUNTED_BACKEND=1"]
async fn mounted_clients_keep_chat_execution_isolated_after_all_windows_close() {
    mounted_backend::check(
        std::path::Path::new(env!("CARGO_BIN_EXE_rovik-desktopd")),
        None,
    )
    .await;
}
