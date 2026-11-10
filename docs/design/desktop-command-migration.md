# Desktop command migration

The shared daemon and authenticated typed cloud RPC exist. The app still
explicitly selects an embedded backend until every service and its event
transport can attach safely. Never fall back to an embedded backend after an
IPC failure.

Desktop-only operations use a separate, versioned local request enum in
`rovik-client::desktop_api`. The backend serializes its existing result types;
the window forwards that JSON unchanged to preserve the frontend contract.
Inputs remain typed and explicitly dispatched. Cloud requests keep their
existing protocol and routing. Native window actions stay in Tauri.

## Commit and push phases

1. **Completed (`be2bb55`)**: Overlay operations and non-streaming VFS mount, file, and history commands.
   Preserve remote imported-result behavior and idle checks. Validate embedded
   and real-daemon calls, errors, and connection reuse.
2. **Completed (`c4879f7`)**: Chat data, memory, non-browser MCP, host, and settings request/reply operations.
   Preserve credentials handling, remote routing, and local mirror reads.
3. **Completed**: Per-client project registration, filesystem and execution ownership, including
   terminal controls. Separate
   native window callbacks from backend services.
4. **Completed**: Subscription-dependent adapters with the event transport:
   terminal output, filesystem watches, VFS subscriptions, streamed model
   replies, host setup progress, and native callbacks. Scope subscriptions to
   clients and clean them up on disconnect without stopping shared work.

Each phase must pass its regression tests and the repository checks before
commit and push. Event-dependent commands must not silently lose callbacks or
pretend they are ready for IPC. The UI remains embedded until these are complete.

## Following this migration

Automatic daemon packaging/start/attach; command-session reconstruction after
failure; lifecycle, crash, and upgrade behavior; Windows named pipes; real
multi-window FUSE and execution isolation tests.

## Private event transport prerequisite

VFS subscriptions now use client sessions and a separate authenticated event
connection, so output does not wait behind an RPC call. Embedded clients use
the same resource ownership and callback routing. Disconnect and callback-loss
cleanup cannot cancel another client's resources. Queues are bounded by event
count and bytes; overflow fails the session explicitly. This is the foundation
for phases 3 and 4, not global agent event replay or reconnect recovery.

## Terminal migration

Local PTYs and remote terminal launches now live in `rovik-client::terminal`.
Native commands forward requests and output only. The backend owns the terminal
registry and shutdown; client leases own individual terminals. Write and resize
require the owning session, and a close from another client is harmless. Existing
UTF-8 decoding, bash setup, workload tracking, and process-tree cleanup are
preserved. Filesystem, project registration, and finite event adapters are covered below.

## Filesystem migration

File tree mutations, editor reads and guarded writes, binary image previews,
last-project settings, and watches now use desktop RPC. File-ID maps, ignored-path
caches, and watch generations belong to client sessions. Watch callbacks and
cleanup use generation checks so rapid project switches cannot stop a replacement
watch. The Tauri adapter delivers filesystem events to the requesting window.
Project registration and the remaining chat/native callback adapters are covered below.

## Finite operations and native callbacks

One-off model completions, host-test progress, MCP OAuth sign-in, and terminal
writes now run as client-owned backend tasks. Their startup replies release the
request connection immediately, so Stop and terminal Close remain callable while
work is pending. Progress and final results use the private event stream in order;
callback loss or disconnect cancels only that client's task. Completion request
IDs are scoped to the client. OAuth credentials remain in the backend; only the
browser URL is forwarded to the native adapter. WSL setup and shared UI settings
also use typed desktop requests. Local OAuth and streamed HTTP fixtures exercise
the same contracts through embedded clients and real daemon connections.

Shared event replay is implemented below. Automatic daemon activation remains
a separate follow-up phase.

## Project ownership and remaining chat operations

Open, copy/move answers, relink, delete, execution-lease queries, and attachment
reads now use the backend. Opens and destructive project operations share a
backend lock, so another client cannot open a project between its ownership check
and mutation. Registration belongs to the client lease; switching folders or
losing that client releases only its entry. Native OS claims and window focusing
remain in the UI adapter.

Remote results and steering resolve the chat's project instead of a global last
opened folder. Memory synchronization uses its explicit root. Delivery tests use
the requesting client's folder. Ambiguous copies are refused rather than choosing
a destination. Embedded and real-daemon tests cover project guards, disconnect
cleanup, and binary attachment round trips; remote tests cover project-specific
steering and delivery tests.

The command families listed in this migration are routed. This does not enable
automatic daemon attachment: command/private-session recovery after failure,
remaining native service ownership (such as browser integration), daemon
packaging/lifecycle, Windows transport, and real multi-window FUSE validation are
still separate work before every window can run purely as a UI client.

## Shared event routing and recovery

Core and cloud events are now recorded before optional native observers in a
single ordered journal. Each UI polls through a separate read-only connection;
this connection can reconnect and replay without reissuing a command or affecting
PTYs and private callbacks. The app consumes this path even in embedded mode.

A scope selects a project (including the no-project view) plus explicitly retained
chats. Child chats follow their lead's project. Settings and host-wide events go
to every reader. Cursors include the backend incarnation, sequence, and scope.
History is bounded to 4,096 events and 8 MiB. Initial attach, changed scope,
restarted backend, or an evicted cursor requires a refresh, not silent loss.

The UI advances a cursor only after applying its batch or successfully refreshing
state. Recovery reads scoped run/approval/question state, reloads cached messages
and summaries, removes deleted chats and stale live state, and refreshes hosts and
ports. Events produced during recovery remain available after its checkpoint.
Replies for an old scope or disposed reader are ignored. Failed reads retry with
the previous checkpoint; partially applied batches force a refresh.

This is recovery of the shared event reader. A backend crash can also invalidate
the command/private-resource session; automatic reconstruction of that session,
project claims, watches and terminals remains part of daemon lifecycle work.
Recovery failures remain visible and do not acknowledge an incomplete refresh.
The UI still explicitly selects embedded mode. Automatic daemon activation,
remaining native service ownership, Windows transport, and mounted multi-window
FUSE validation remain outstanding.

## Git service ownership

All 35 Git panel commands now use typed desktop RPC. The implementation and its
regression tests live in `rovik-client`; Tauri only forwards inputs and replies.
Commit hooks, credentials, trash handling, merge/rebase, and working-tree busy
guards retain their existing behavior while executing in the backend process.
HTTPS/SSH support and Intel macOS OpenSSL linkage also belong to the standalone
backend. History cancellation counters are client-scoped, so searching in one
window cannot supersede another window's history scan.

Embedded and real-daemon tests cover two clients and separate repositories,
commit/status/history, stash round trips, branch changes, path errors, and reuse
after a failed request. Existing Git service tests move with the implementation.
Browser integration and other remaining native services still need migration before
automatic daemon attachment is enabled. The app continues to use embedded mode.

## Language-server ownership

LSP start, send, stop, and rust-analyzer discovery/install/toolchain commands now
use desktop RPC. Server controls require the owning client; output is reserved
before spawn and uses the bounded private stream. Pipe writes and setup commands
use finite tasks, leaving Stop callable while stdin is blocked. Natural exit is
reported after stdout/stderr drain. If a detached helper keeps a pipe open, the
backend bounds the drain, closes the reader, and reports it explicitly. Disconnect, callback loss, startup failure,
and backend shutdown stop the process tree; shutdown waits for output workers.

The backend uses its existing data directory for managed rust-analyzer binaries.
Installations serialize download and publication, including blocking work that
outlives its requesting task. Failed staging directories are removed. Older
published versions are retained because another window can still hold their path.
Automatic pruning needs a later version-lease/lifecycle policy.

Embedded and real-daemon fixtures cover early output, Unicode framing, execution
in the backend process, cross-client controls, disconnect/callback cleanup,
child-process termination, natural exit ordering, and Stop under stdin
backpressure. Browser integration and other native services remain before
automatic daemon attachment; the app still selects embedded mode.

## Headless browser commands and Chrome installation

Web search, page reads, screenshots, browser status, and installation now use
typed desktop RPC. The native adapters only forward arguments, results, and
private progress. Browser work runs as finite tasks, so a browser call does not
hold the command connection. Existing URL/network checks, the serialized shared
headless browser, and screenshot attachment storage remain in the core service.

Concurrent installation requests share one backend-owned job. Progress is
coalesced for each observer; late observers receive the current progress.
Disconnect or callback loss removes that observer without dropping an archive or
installation lock during blocking unpack. Completion (including panic/failure)
is reported explicitly, subsequent requests can retry/recheck the executable,
and shutdown waits for accepted publication before ending the core services.

Unit fixtures cover observer loss, shared work, retry, and shutdown. Embedded and
real-daemon checks cover status, forbidden URL schemes/private destinations,
empty searches, and connection reuse after errors. They need no external browser
download. Live Chrome navigation/download is not exercised by these fixtures.

## Remote page callbacks

Forwarded-page opening now uses a private, session-owned desktop RPC callback.
The backend selects the unique window viewing the chat's project, falling back
to a window retaining its chat or lead. Missing or ambiguous owners produce the
existing chat notice instead of opening multiple windows. Child chats resolve
through their lead, and each request includes its current forwarding metadata.
Page actions are never journaled or replayed. Project switches and disposal
invalidate queued UI callbacks; late subscription replies are released. Callback
acceptance means queued delivery, not confirmation that the OS opened a browser.

Unit and remote forwarding fixtures cover routing, ownership, ambiguity,
disconnect, replacement, and forwarding metadata. Embedded and daemon fixtures
cover subscription lifecycle, cross-client rejection, and reuse after errors.
Frontend tests cover stale callbacks, late replies, metadata ordering, and OS
opener failures.

Remaining native startup hooks still need migration.
Automatic daemon activation, session reconstruction, Windows transport, and
mounted multi-window FUSE validation remain. The app still selects embedded mode.

## Editor formatter ownership

Formatter discovery and execution now use typed desktop RPC. Project-local tools,
configuration search boundaries, Rust editions, PATH fallback, and buffer/error
shapes retain their existing contracts. The backend formats the unsaved buffer
and returns text without writing the source file. Both discovery and execution
run as finite tasks, leaving the request connection available to other commands.

Each accepted worker stays registered until cleanup completes. Losing its client
cancels only that client's formatter. Backend shutdown rejects new workers,
cancels existing work, and waits for cleanup. Formatter stdin/stdout/stderr use
cancellable I/O rather than detached reader threads; cancellation, timeouts, and
output-limit failures close the pipes and kill/reap the wrapper's process tree.

Existing formatter regression tests moved with the implementation. New tests
cover blocked input during shutdown and prompt rejection of excessive output.
Embedded and real-daemon fixtures verify backend process execution, different
project-local formatters in two clients, unsaved-buffer results, unchanged source
files, useful errors and connection reuse, and disconnect cleanup while another
client continues formatting. Platform-specific Windows wrapper tests are retained;
this environment validates the Linux paths.

Native startup hooks remain before automatic daemon attachment.
Session reconstruction, Windows transport, and mounted multi-window FUSE
validation also remain; the app continues to select embedded mode.

## Workload queue ownership

Focus, slot-limit settings, and queue status now use typed desktop RPC. Slot
limits are shared and persisted by the backend, with serialized writes retaining
the existing automatic mode and 1–8 clamping. Status also reports the effective
focused chat.

Each client retains its own visible-chat selection. The most recently reported
selection wins; returning to a window promotes its chat again. Hiding or closing
a chat releases only that client's selection, falling back to the most recent
remaining client. Session disconnect performs the same cleanup, and a closed
session cannot register a late selection. The UI serializes and coalesces its
reports so delayed replies cannot restore a selection after component disposal.

Local priority changes immediately. A single backend worker coalesces pending
cloud updates and sends them in order, independently of UI commands. Reconnecting
hosts receive the latest selection, including an explicit clear. Host failures
are logged, as with other shared host settings. Backend shutdown clears local
priority, sends the final cloud clear, and joins the sender; host calls retain
the existing bounded timeout.

Unit and frontend tests cover selection ownership, disconnect/disposal cleanup,
late calls, promotion, and coalescing behind slow work. Remote fixtures cover
reconnect snapshots. Embedded and real-daemon fixtures verify shared limits,
automatic mode, clamping, priority fallback, and continued commands after another
client disconnects.

Remaining work: native startup hooks, automatic daemon attachment and session
reconstruction, Windows transport, and mounted multi-window FUSE validation.
The app still selects embedded mode.
