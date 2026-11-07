# verified-code-memory

Evidence-gated long-term memory for coding agents.

## v0.2 improvements

- SQLite FTS5/BM25 lexical retrieval with safely quoted queries
- Retrieval telemetry: candidates, FTS matches, file matches, rejected candidates,
  returned records, latency
- Repository-version invalidation for stale memories
- Conflict resolution: keep one verified memory and deprecate contradictions
- Pluggable `MemoryEmbedder` and `VectorIndex` traits
- Thread-safe `SharedMemoryStore`
- Integration tests, CI, docs, and runnable example

## Retrieval

Two pools feed one ranked answer, and neither stands in for the other: the
memories that share a *specific* word with the task or the failure (the full-text
index), and the memories that name one of the task's files (`src/queue.rs` and
`queue.rs` are the same file; `foo.rs` and `src/notfoo.rs` are not). Files are
looked up through a `memory_files` table of normalized paths, written with each
record, so "which memories are about this path" is an indexed lookup rather than
a scan that parses every record's JSON. A store written before either index
existed is brought up to date when it opens: the file rows are filled from the
records, and an empty full-text index is refilled from the table.

Relevance admits a memory; trust does not. A memory that shares no specific word,
no file and no failure term is not retrieved at all, so a query about something
the project has no memory of answers with nothing instead of with its most
trusted record. Verification state, kind, recency and past successes only order
the memories that already belong to the query.

`RetrievalQuery` carries the caller's own limits — the project, the trust states
it accepts, the tags it excludes (a chat doesn't recall its own work) — and they
are applied before the result limit, so a record the caller may not see can't
take a slot from one it may. Deprecated memories are never retrieved; `list` is
where they show.

SQLite's `bm25()` is negative and the better a row matches the more negative it
is. `bm25_relevance` keeps that direction (`rank = -4.0` scores above `-0.5`)
and bounds it to `0..1` before the contextual reranker adds its components.

`render_packet(memories, tokens)` renders what a prompt or a tool answer shows: at
most `tokens`' worth (`estimate_tokens`, four characters to a token), each record
with its id — what `memory_get` takes — its applicability, its evidence and its
files. A record that would not fit ends the packet with a line saying so and a
pointer to fetch it, rather than being dropped silently; `render_context` is the
same rendering with no bound.

## Evidence, applicability and freshness

Three questions a record answers, kept apart because they have different answers:

- **verification** — what evidence once supported the claim;
- **applicability** — whether that is expected to hold for the code as it is now
  (`current`, `needs_revalidation`, `unknown`);
- **provenance** — what kind of source asserted it (user assertion, code
  observation, test result, generated deduction).

`verify(id, evidence)` is claim-scoped: it records the claim hash of the text the
evidence was about, so an edit cannot inherit trust — and a summary of that:

- editing a memory keeps its verification and evidence for audit, marks the claim
  `needs_revalidation` and the source as the user's assertion;
- `mark_needs_revalidation` is what a nearby failure or a changed revision earns,
  never `deprecate`: only a contradiction deprecates;
- `mark_stale_for_version` asks for revalidation rather than invalidating every
  versioned record on a new commit;
- `record_outcome` counts a use and nothing else, and `record_retrieval` stamps
  `last_retrieved_at`; recency reads `content_updated_at`, the claim's own stamp,
  so no amount of recall or association makes a claim look newer.

A store file written by an earlier build of this crate is brought up to date when
it opens: missing columns are added, claim hashes backfilled from the records,
the file index filled, and then — only after that — the full-text index created
and refilled. The order matters: FTS5 keeps an external-content index in step
through the triggers, and a trigger firing at a row an empty index never held is
what SQLite reports as `SQLITE_CORRUPT_VTAB`.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo doc --no-deps
cargo run --example basic
```
