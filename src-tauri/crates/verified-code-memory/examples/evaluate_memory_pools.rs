//! Evaluates duplicate and visibility pressure on candidate pools.
#[path = "../tests/support/pools.rs"]
mod pools;

fn main() {
    println!(
        "{}",
        serde_json::to_string_pretty(&pools::evaluate()).unwrap()
    );
}
