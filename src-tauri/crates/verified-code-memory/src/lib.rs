#![deny(missing_docs)]
#![forbid(unsafe_code)]

//! VerifiedCodeMemory: evidence-gated long-term memory for coding agents.
//!
//! Records begin provisional and are promoted only with execution or review
//! evidence. Retrieval combines SQLite FTS5/BM25 lexical search with a
//! deterministic contextual reranker and returns telemetry for evaluation.
//!
//! ```no_run
//! use verified_code_memory::{MemoryStore, NewMemory, RetrievalQuery};
//!
//! # fn main() -> verified_code_memory::Result<()> {
//! let store = MemoryStore::open("agent-memory.db")?;
//! let memory = store.remember(
//!     NewMemory::episode("repo", "Retry race", "Serialize retries by order ID.")
//!         .with_files(["src/retry.py"]),
//! )?;
//! store.verify(memory.id, "cargo test passed")?;
//!
//! let (memories, telemetry) = store.retrieve_with_telemetry(
//!     &RetrievalQuery::new("repo", "fix retry race").with_files(["src/retry.py"]),
//! )?;
//! println!("{} memories in {} ms", memories.len(), telemetry.elapsed_ms);
//! # Ok(())
//! # }
//! ```

pub mod embedding;
pub mod error;
pub mod model;
pub mod retrieval;
pub mod store;

pub use embedding::{MemoryEmbedder, VectorIndex};
pub use error::{Result, VcmError};
pub use model::{
    MemoryKind, MemoryRecord, MemoryStats, NewMemory, RetrievalQuery, RetrievedMemory, Verification,
};
pub use retrieval::{MemoryScore, RetrievalTelemetry, RetrievalWeights};
pub use store::{MemoryStore, SharedMemoryStore};
