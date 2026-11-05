use verified_code_memory::{MemoryStore, NewMemory, RetrievalQuery};

fn main() -> verified_code_memory::Result<()> {
    let store = MemoryStore::open("agent-memory.db")?;

    let memory = store.remember(
        NewMemory::episode(
            "payments-service",
            "Checkout retry race condition",
            "Serialize retries by order ID.",
        )
        .with_files(["src/checkout/retry.py"]),
    )?;

    store.verify(memory.id, "cargo test --workspace passed")?;

    let (memories, telemetry) = store.retrieve_with_telemetry(
        &RetrievalQuery::new("payments-service", "fix checkout retry race condition")
            .with_files(["src/checkout/retry.py"]),
    )?;

    println!("{}", store.render_context(&memories));
    println!("telemetry: {telemetry:?}");
    Ok(())
}
