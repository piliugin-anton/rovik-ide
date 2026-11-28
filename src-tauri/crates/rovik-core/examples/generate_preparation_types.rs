//! Explicitly print the frontend preparation contract; tests never update it.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    print!(
        "{}",
        rovik_core::harness::ide::typescript_preparation_contract()?
    );
    Ok(())
}
