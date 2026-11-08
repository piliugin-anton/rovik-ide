//! Evaluates packet capacity independently of semantic recall.
#[path = "../tests/support/capacity.rs"]
mod capacity;

fn main() {
    println!(
        "{}",
        serde_json::to_string_pretty(&capacity::evaluate()).unwrap()
    );
}
