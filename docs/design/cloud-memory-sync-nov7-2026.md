# Project memory synchronization with cloud hosts

The desktop and each compatible host keep durable replicas of a project's
memories. Cloud worktrees bind to a stable project ID, so separate chats on the
same host consult the same history. The desktop retains its existing folder-based
store; repository keys are remapped at the transport boundary. No desktop path is
used to open a server database.

## Exchange and lifecycle

`memory-sync` capability negotiation protects older daemons. `MemorySync` calls
are authenticated against the registered project's owning computer. The memory
library owns the versioned page schema; the protocol carries its JSON without
depending on SQLite or the memory implementation.

Exchanges discover the remote database identity, merge changes, and persist
acknowledged cursors. A replaced database resets the cursors and receives the
complete local history. Each page contains at most 64 revisions and 512 KiB.
SQLite triggers track material changes, including bulk validation, supersession
and pruning. Retrieval timestamps and usage counters alone create no traffic.
Duplicate delivery is idempotent; a failed call is retried from durable progress.
Each host/project exchange is serialized and bounded by a 30-second deadline.
Partial progress survives timeout or process restart.

Synchronization runs before a cloud start, while carrying a project, after remote
run completion, on reconnect, before moving a chat back, and when refreshing or
changing memory in Settings. Code snapshot caching does not suppress memory
exchange. Disconnected hosts catch up when they reconnect. A background exchange
handles run completion so other chat events can continue arriving. Memory sync
failures produce warnings; local manager reads and ordinary starts remain
available. A manager mutation is saved locally first, with an explicit message
if delivery to a connected host fails.

Deletes retain tombstones and dominate edits of that ID. Clearing advances a
project epoch: a host returning with older, even previously unseen, history
cannot restore it. A new claim after receiving the clear belongs to the new
epoch. Imported removals and replacements invalidate existing run write leases,
so a paused or delayed run cannot undo them.

Concurrent changes resolve using logical revisions, independently of host clock
skew. The claim has a separate revision from its verification metadata: a busy
offline host validating old text cannot overwrite a newer user correction.
Concurrent edits of the text use a deterministic replica tie-breaker. Edits retain the evidence binding to the original claim and therefore need
revalidation. Synchronization does not verify, promote or make a claim current.
Source-episode IDs, original evidence, file associations and record IDs survive
exchange. Different observations are not collapsed into one claim. Embeddings
and other derived indexes stay local and rebuild from the receiving records.
Usage counts are not a globally summed measure across machines.

## Scope, settings and existing histories

The daemon resolves project IDs through its own registered state and rejects
other computers and unknown projects. Claims tied to restricted files are omitted
from exports; tombstones and clear epochs still propagate. Changing desktop file
restrictions forces a fresh scan of eligible records. Previously transferred
records remain subject to the run's current visibility rules.

Memory enable/capture and semantic settings continue to load from the actual
worktree's settings folder, separately from the stable storage identity. Optional
semantic model files and interpreters must be provisioned on each machine; they
are not transferred as memories.

Existing cloud per-worktree stores migrate into the stable project store with
preserved IDs and durable migration progress. Original files remain available.
Restarting the daemon or revisiting an old worktree cannot reintroduce cleared
history. A local folder and its registered project mapping must be available
for desktop/host synchronization. Hosts need an updated daemon advertising
`memory-sync`; older hosts continue using their existing protocol.

## Acceptance review

| Requirement                              | Implementation and regression coverage                                                                                              |
| ---------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| Desktop and cloud exchange real claims   | Client exchange, daemon ownership gate, real-daemon scripted agent records a claim which appears in the desktop store.              |
| Cloud chats share history                | Stable project binding; a second cloud chat receives an existing verified claim and reads the first chat's finding.                 |
| Edits preserve trust semantics           | Complete record replication retains claim/evidence hashes and source links; edited claims require revalidation.                     |
| Delete/clear cannot resurrect history    | Tombstone and epoch store tests, stale run lease checks, real-daemon offline clear/reconnect.                                       |
| Reconnect and replaced databases recover | Durable cursors, identity handshake, real-daemon restart and database replacement tests.                                            |
| Existing cloud histories survive         | Per-worktree migration tests using two real cloud worktrees; restart-after-clear regression.                                        |
| Scope and permissions remain enforced    | Cross-project ID collision rollback, another authenticated computer refused, unknown project refused, restricted export regression. |
| Older hosts remain compatible            | Capability-gated client test asserts no new request is sent to an older fake daemon.                                                |
| Bounded transfer and retries             | Paginated replay, concurrent edits between pages, atomic malformed-page rejection, durable progress and idempotency tests.          |

## Validation at the requested push

The user requested an immediate commit and push without further builds. This
commit is therefore not a claim that its final source passed every CI gate.
Completed checks during implementation include:

- 2,600 core nextest cases passed, with 27 existing skips.
- Five real-daemon memory-sync integration cases passed.
- Ten store synchronization cases passed, including the regression preventing
  late verification from overwriting corrected text.
- The initial combined library run passed 345 cases and failed two protocol
  coverage checks because the new variants lacked serialization samples. Those
  samples have been added; the corrected checks have not been rerun.
- Frontend tests, type checking and `format:check` passed after rebasing.

The full daemon build exhausted temporary storage while linking all integration
executables together. The desktop build, doctests and Clippy also stopped on
that storage limit. Running daemon targets individually was prepared but was
not started after the instruction to stop building. The final durable-host
rediscovery changes, sixth daemon regression and byte-boundary regression still
need validation, along with the corrected protocol samples. Native Windows,
macOS and the Landlock matrix have not been run for this feature.
