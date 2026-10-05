fn main() {
    // winfsp.dll is loaded on first use, so the app starts without WinFsp.
    #[cfg(windows)]
    winfsp::build::winfsp_link_delayload();
    tauri_build::build()
}
