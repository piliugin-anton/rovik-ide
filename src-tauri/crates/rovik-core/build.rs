fn main() {
    // winfsp.dll is loaded on first use, so tests start without WinFsp.
    #[cfg(windows)]
    winfsp::build::winfsp_link_delayload();
}
