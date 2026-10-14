//! The agent without a window (rovik-core's headless.rs). Build with
//! `cargo run -p rovik-headless -- --help`.

fn main() {
    std::process::exit(rovik_core::headless::main_cli());
}
