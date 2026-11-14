//! Standalone entry point for the same host used by the desktop executable.
#[cfg(any(unix, windows))]
fn main() {
    rovik_client::local_backend::host::init_log();
    rovik_core::process::adopt_login_shell_path();
    rovik_core::overlay::confine::require();
    let result = rovik_client::local_backend::host::paths(std::env::args_os().skip(1))
        .map_err(|error| -> Box<dyn std::error::Error> { error.into() })
        .and_then(|paths| {
            rovik_core::rt::block_on(rovik_client::local_backend::host::serve(paths))
        });
    if let Err(error) = result {
        eprintln!("Local backend: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(any(unix, windows)))]
fn main() {
    eprintln!("Local backend hosting is unsupported on this platform.");
    std::process::exit(1);
}
