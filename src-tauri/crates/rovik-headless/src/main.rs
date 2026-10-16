//! The agent without a window (src/lib.rs). Build with
//! `cargo run -p rovik-headless -- --help`.

fn main() {
    std::process::exit(rovik_headless::main_cli());
}
