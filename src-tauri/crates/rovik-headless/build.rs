fn main() {
    // winfsp.dll is loaded on first use, so the runner starts without WinFsp.
    #[cfg(windows)]
    winfsp::build::winfsp_link_delayload();
}
