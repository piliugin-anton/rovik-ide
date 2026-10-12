//! The agent without a window (src/headless.rs). Build with
//! `cargo run --features headless --bin rovik-headless -- --help`.

fn main() {
    std::process::exit(rovik_ide_lib::headless::main_cli());
}
