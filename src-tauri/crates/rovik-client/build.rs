fn main() {
    // winfsp.dll is loaded on first use, so tests start without WinFsp: they
    // link the core, which uses it.
    #[cfg(windows)]
    winfsp::build::winfsp_link_delayload();
}
