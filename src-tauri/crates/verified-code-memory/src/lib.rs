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

/// Pluggable embeddings, for ranking memories by meaning (optional).
pub mod embedding;
/// The crate's error type.
pub mod error;
/// What a memory is: its record, its input, and a retrieval request.
pub mod model;
/// Ranking retrieved memories, and the telemetry of a retrieval.
pub mod retrieval;
/// The SQLite store: records, evidence, retrieval and counts.
pub mod store;

pub use embedding::{MemoryEmbedder, VectorIndex};
pub use error::{Result, VcmError};
pub use model::{
    claim_hash, Applicability, MemoryKind, MemoryRecord, MemoryStats, NewMemory, RetrievalQuery,
    RetrievedMemory, SourceKind, Verification,
};
pub use retrieval::{estimate_tokens, MemoryScore, RetrievalTelemetry, RetrievalWeights};
pub use store::{MemoryStore, SharedMemoryStore};

/// Optional persistent semantic index and bounded hybrid ranking.
pub mod semantic;
