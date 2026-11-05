# verified-code-memory

Evidence-gated long-term memory for coding agents.

## v0.2 improvements

- SQLite FTS5/BM25 lexical retrieval with safely quoted queries
- Retrieval telemetry: candidates, FTS matches, returned records, latency
- Repository-version invalidation for stale memories
- Conflict resolution: keep one verified memory and deprecate contradictions
- Pluggable `MemoryEmbedder` and `VectorIndex` traits
- Thread-safe `SharedMemoryStore`
- Integration tests, CI, docs, and runnable example

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo doc --no-deps
cargo run --example basic
```

FTS5 ranks BM25 matches with lower values indicating stronger relevance; the
store normalizes that value into a positive lexical component before contextual
reranking. [web:87][web:91]
