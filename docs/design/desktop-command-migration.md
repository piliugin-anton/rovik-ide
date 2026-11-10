# Desktop command migration

Unix desktop windows now attach automatically to the shared daemon. Windows
retains an embedded host pending named-pipe transport. Every migrated service
and its event transport can attach through authenticated local RPC. Never fall
back to an embedded backend after an IPC failure.

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
5. **Completed (`af41d6a`)**: Backend project guards and explicit client detachment.
6. **Completed**: Automatic Unix attachment through the packaged executable,
   shared host startup, native focus separation, and updater network configuration.
7. **Completed**: Explicit Unix command-session recovery with guarded project registration,
   UI resource reconstruction, and stale-response rejection.
8. **Completed**: Guarded Unix backend upgrades with independent control negotiation,
   retained-work checks, serialized launchers, and ownership through shutdown.
9. **Completed**: Required Linux mounted multi-client regression for standalone and
   packaged hosts, including isolated execution across client detachment.
10. **Completed**: Drain empty, unused Linux virtual volumes during guarded upgrades;
    preserve busy mounts and retained files on refusal.
11. **Completed**: Drain idle top-level Linux chat views with checked persistence,
    preserving changes across replacement and retaining busy or unsaved views.

12. **Completed**: Drain idle remote connections during Unix upgrades, joining
    event receivers and retaining queued imports and detached event work.

13. **Completed**: Drain dependent Linux chat views children first, retaining
    busy ancestors and reconstructing saved descendants after refusal or replacement.

14. **Completed**: Allow persisted inactive remote mirrors through Unix replacement,
    checking ownership before and after draining without clearing live routing.

15. **Completed**: Persist remote Keep/recheck decisions with checked atomic writes,
    restart restoration, explicit failures, and upgrade checkpoints.

16. **Completed**: Drain local remote-work queues before pausing results and detach
    verified server-owned runs/drivers for restoration by the replacement.

Each phase must pass its regression tests and the repository checks before
commit and push. Event-dependent commands must not silently lose callbacks or
pretend they are ready for IPC. Windows remains embedded until its transport is ready.

## Following this migration

Windows named pipes; draining child chat views and remote resources for
upgrades; macOS virtual-volume draining; GUI multi-window smoke testing and macOS mounted validation. Linux
mounted multi-client execution is covered by the required contract below.

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

## Backend host startup and maintenance

Host connection startup, developer socket/chat configuration, and periodic host
refresh now share one backend-owned worker in both embedded and daemon hosts.
The host calls `start_services` after configuring its ownership policy; repeated
calls do not start another worker. Credential and filesystem work stays off the
UI and RPC threads. Refreshes never overlap, and shutdown stops scheduling before
joining any accepted blocking startup or refresh work. A stopped backend cannot
restart maintenance. Worker failures are reported on stderr, including when the
daemon has no application logger.

The daemon now honors `ROVIK_REMOTE_CHATS`, `ROVIK_COMPUTER`, and
`ROVIK_REMOTE_TOKEN`, as embedded startup already did. Those values belong to the
backend process environment; attaching another window does not reapply them.
The UI no longer registers unused Core, Remote, host, key, or credential service
aliases as Tauri state.

Tests cover startup-once behavior, shutdown during startup and refresh, restart
rejection, and the developer environment configuration in a real daemon, including
persisted chat placement after graceful termination. Existing embedded and daemon
RPC fixtures run alongside these checks.

Project-window focus/claims and the retained-project UI loop remain native. The
embedded cross-process ownership callback must be replaced when shared daemon
attachment becomes the default; window exit must then release its client lease
rather than stop the shared backend. Automatic attachment/session reconstruction,
Windows transport, and mounted multi-window FUSE validation remain outstanding.

## Backend ownership checks and window detachment

Project mutation guards now run entirely in the backend. Shared hosts use the
client-session project registry; native focus locks cannot suppress shared remote
events. Private embedded hosts explicitly configure a compatibility policy that
reads existing cross-process project claims. The native focus service and that
policy share the same metadata format and owner tokens. The AppHandle ownership
callback and duplicate native mutation guards are removed.

`DesktopConnection::close` releases only its session, including project and focus
registrations, terminals, subscriptions, and finite UI tasks. It rejects further
commands and journal reads. Native exit detaches this connection first; only an
explicit embedded-host state can then shut down a private backend. The app still
selects embedded mode, so closing its current window still ends that private host.

Local protocol 17 adds an authenticated, idempotent session-close control on an
independent connection. Detachment interrupts a pending client reply without
replaying the accepted operation, and acknowledges session release while owned
resources finish closing. Closing also interrupts idle or partial frame reads, so
a still-open socket cannot submit new commands. An accepted backend mutation may
still finish. Failure to obtain the cleanup acknowledgement is reported and close
can be retried. Closing during event-stream
startup cannot revive callbacks or register new resources.

Regression coverage exercises embedded and real-daemon project release, closed
client rejection, LSP process cleanup while another client continues working,
and daemon reconnection without a new boot. Unit tests cover pending RPC reply
cancellation, event-subscription races, and embedded ownership tokens, stale
claims, missing metadata, and remote event routing.

Next: automatic daemon attachment and client session reconstruction, then Windows
transport and mounted multi-window FUSE validation. Native focus/position handling
stays in the UI; the retained-project UI loop needs adjustment when attachment is
enabled. This phase does not claim mounted FUSE or native multi-window validation.

## Automatic desktop attachment on Unix

Linux and macOS windows now discover and attach to the shared backend by default.
When discovery is absent, they launch the packaged Rovik executable with
`--desktop-backend --data <absolute-directory> --config <absolute-directory>`.
This entry point runs before Tauri or webview initialization and shares the host
implementation with `rovik-desktopd`. No separately bundled sidecar is needed.
AppImages launch the original image, keeping its libraries mounted independently
of the window that started it. Launchers use independent process sessions and
close inherited descriptors; their children are reaped without tying host lifetime
to a window.

The lifetime lock still arbitrates simultaneous startup before Core, databases,
or mounts are initialized. Authentication and incompatible-protocol errors never
cause an embedded fallback. A failed launcher is reported immediately; successful
losing launchers wait for the winner's readiness. The initial authenticated
connection becomes the window's command session after a dispatcher acknowledgement.

Native window slots are separate from execution slots. Focus claims follow only
the visible project; changing projects no longer retains a native claim just
because a backend chat is running. Closing the last window detaches its session
without stopping execution or mounts. The next window attaches to the same host.
Windows retains its explicit embedded host until named-pipe transport is ready.

The native updater obtains a resolved HTTP configuration through authenticated
RPC, preserving configured proxy authentication, local bypass, and timeout behavior
without initializing a second credential store in the UI. Credentials remain in
native memory and are redacted from Debug output. Restarting a Unix UI for an update
no longer warns that it will stop backend chats.

Editor marker reports and preview edit/write commands also cross RPC, so the
agent sees diagnostics in its own process and mutations share its file guards.
Disconnect removes that client's diagnostic reports while preserving newer reports
from another client. Late reports from a closed session are rejected.
Read-only editor helpers (typings, source outlines, project-file enumeration, and
preview reads/search) remain UI-local and do not own execution services.

Local protocol 18 adds these requests and the native HTTP configuration. An incompatible already-running
host must be stopped explicitly after its work finishes before opening the new
app; a window restart alone does not replace it. Automatic safe host replacement
and client session reconstruction remain the next lifecycle work. Commands are
never automatically replayed after a disconnect.

Validation covers launcher failures, existing-host attachment, incompatible-host
refusal, concurrent automatic attachment, reuse after all clients close, native
proxy snapshot behavior, and the existing embedded/daemon command contracts.
Windows transport and mounted multi-window FUSE validation remain outstanding.

## Explicit command-session recovery

The connection notification now offers Reconnect. A native window holds a
replaceable session; recovery detaches the old session, discovers or launches the
host, and validates the existing project identity/path before publishing the new
session. It never resolves a copy/move, merges history, or retries an uncertain
command. A rejected project snapshot leaves commands unavailable until a valid
recovery. Concurrent recovery operations are serialized, and closing a window
interrupts recovery so it cannot publish another live session afterward.

The UI coalesces reconnect clicks and keeps its editor models and composer state.
If the project changes while attachment is pending, only session setup is repeated
for the latest context. Recovery refreshes chat/run/approval/host state, restores
file identities and watches, re-registers page/VFS subscriptions and workload
focus, resends diagnostics, and restarts language support from open models. A new
host remounts virtual files when a driver is installed. Terminal views receive an
exit; no shell or terminal job is replayed. Finite tasks lose their old callbacks
and report failure rather than being rerun.

Generations reject late terminal output, startup replies, VFS callbacks, and
journal snapshots from abandoned sessions. A late numeric VFS subscription ID
cannot unsubscribe a new session's reused ID. Old and new native calls remain
pinned to their own session throughout each await. Restore failures remain
visible and can be retried explicitly; mutations are never automatically replayed.

Local protocol 19 adds guarded project restoration. An incompatible older host
still requires an explicit stop after its work finishes. Safe automatic backend
replacement during upgrades, Windows transport, and real multi-window mounted
FUSE validation remain separate phases. Recovery is implemented for Unix shared
hosts; Windows continues to recover its embedded host by restarting the window.

Validation covers reconnect coalescing, changing project context, failed setup and
restoration, close during recovery, same-host project ownership, abrupt host death
and a new boot, and stale event/resource replies. Both the standalone daemon and
the native executable run the attachment/recovery contract.

## Guarded Unix backend upgrades

Local protocol 20 introduces a separate `control.sock` with stable control
protocol 1. It negotiates replacement independently of the desktop command
protocol, authenticates same-user peers, bounds frames and negotiation time, and
binds each request to the running host's boot identity. Version and command
protocol must advance monotonically; an older app cannot retire a newer host.
A persistent `launch.lock` serializes automatic launchers across negotiation,
shutdown, and replacement startup. The old host retains `owner.lock` through all
cleanup and the clean database marker; no launcher kills it or steals its mounts.

Retirement requires no accepted RPC connections, including event streams, pending
handshakes, and requests whose window disappeared. The host temporarily stops
accepting connections, joins and pauses host maintenance and result imports, then
checks retained work. Chats, approvals, connector setup, memory preparation,
background commands and unread results, browser installation, client tasks,
detached blocking work, remote receivers/hosts/mirrors, open chat filesystem views,
mounted VFS volumes, and retained virtual files defer replacement. A refusal
releases the pause guards. Compatible apps continue using the existing host;
incompatible apps report the blocker without launching a second execution owner.

This phase deliberately does not unmount volumes or drain remote work to make an
upgrade possible. Close windows and finish/release retained work before retrying.
Hosts predating control protocol 1 still require an explicit graceful stop for an
incompatible upgrade. Failure or timeout never authorizes forced replacement.
Windows named pipes, automatic draining of mounted/remote resources, and real
multi-window mounted FUSE validation remain follow-up phases.

Validation covers monotonic version/protocol negotiation, stale identities,
bounded frames, maintenance resume and shutdown while paused, detached blocking
work, retained virtual files and mounts, live-client refusal, and a real daemon's
clean retirement followed by a new owner and boot identity.

## Required mounted multi-client isolation

Linux CI explicitly runs the mounted contract against both `rovik-desktopd` and
the packaged executable in headless backend mode. These tests require
`ROVIK_MOUNTED_BACKEND=1` and working FUSE; mount failures fail the CI step rather
than producing a capability skip. Both the default sandbox and Landlock matrix
run this contract. They use a local scripted model and actual sandboxed commands.

Two independent UI-client connections reuse one host and one virtual-files
mount. They keep the existing exclusive project-view policy: a second client
views another folder while requests address chats by their explicit identities.
Two chats on the same project write the same filename through distinct kernel
mounts, leaving the project and the other selected folder untouched. Commands
continue after one client closes and after all clients close. An already-open
virtual-file handle remains usable. A new client reuses the same boot and files;
stopping one chat's command leaves the other running. Cleanup discards both
views, unmounts the volume, and verifies a clean host exit and released lease.

Run on a Linux machine with `fuse3` and `/dev/fuse`:

```sh
cd src-tauri
ROVIK_MOUNTED_BACKEND=1 cargo test -p rovik-client --test mounted_backend -- --ignored
ROVIK_MOUNTED_BACKEND=1 cargo test -p rovik-ide --test shared_backend -- --ignored
```

This validates the actual mounted backend boundary used by windows. It does not
launch graphical windows or validate macOS's mount driver. Those smoke tests,
Windows transport, and automatic draining of retained resources remain separate.

## Draining unused Linux virtual volumes

Guarded upgrades can now release an empty virtual-files mount on Linux. The host
first excludes accepted clients, pauses maintenance and remote imports, and checks
all non-mount blockers. Retained virtual files, chat views, remote connections,
running work, and unread command results still defer replacement.

The upgrade path uses an ordinary unmount, without lazy detachment or force.
An open file, directory descriptor, or shell working directory keeps the mount
busy; refusal preserves the serving session and the existing owner. Once the
kernel accepts unmount, a second retained-work check catches files written during
the preflight race. Those bytes remain in the old process even if the volume has
already been unmounted.

The host joins draining before resuming admission, including when the negotiating
client disconnects. Only peer reads and writes have negotiation timeouts; a slow
drain never authorizes a second owner or overlaps new requests. Final shutdown
still retains the ownership lease through cleanup and the clean database marker.

The required mounted contract runs this path through both executables: retained
files refuse retirement, an empty volume with an open directory refuses without
losing its mount, reattachment uses the same boot, and closing the handle permits
clean retirement. Chat-view draining, remote producer draining, and macOS ordinary
unmount remain follow-up work.

## Draining idle Linux chat views

Guarded upgrades also drain idle top-level chat overlays. This closes their
views without landing or discarding changes. Running chats, pending approvals,
background commands and unread results still block retirement before draining
starts. Open child views defer the whole overlay drain because they depend on
their parents' mount paths.

Each view is saved with an error-reporting manifest write, then unmounted without
force or lazy detachment. Busy handles preserve the serving mount. A second save
after unmount captures writes that completed during draining; only then are the
view registration and store lock released. If that save fails, the backend keeps
the overlay and its store lock and attempts to restore the mount. If restoring
also fails, the error reports it, status no longer advertises a mounted view, and
opening the overlay retries mounting the retained state.

A refusal can follow partial progress: already-drained views remain saved and
can be read through the existing host; the busy or unsaved view remains owned.
The next upgrade attempt continues with remaining views. The host keeps admission
closed and producers paused throughout draining.

The mandatory Linux mounted test covers busy chat handles, a failed manifest
write, continued access after refusal, partial draining, clean retirement, and a
new backend reading both chats' saved edits while the project stays unchanged.
A unit test verifies persistence errors retain edits and allow a later successful
save and reopen. Retained remote-work draining, child dependency handling, macOS draining, and
Windows transport remain separate.

## Draining idle remote connections

Unix backend replacement disconnects idle hosts, including offline connections,
without changing saved settings or stopping remote execution. Before disconnecting,
it refuses mirrored chats, transfers, queued results, forwarded ports, and detached
event work such as MCP relays and memory synchronization.

Result jobs remain counted from enqueue through completion, including when the
worker has already dequeued a job and is waiting on the upgrade pause. Detached
event futures register before scheduling, so an unpolled task also blocks
replacement. Both registrations release on completion or cancellation.

After disconnecting, the backend waits up to one second for event receivers to
finish their buffered events. A timeout refuses retirement without aborting them.
A second retained-work check catches jobs or tasks created while receivers drain.
Admission stays closed and maintenance and result imports remain paused throughout.
On refusal, producers resume, queued work remains available, and the next
maintenance refresh reconstructs removed saved connections even when their
settings have not changed.

Regression tests cover idle and offline hosts, retained mirrors, receiver timeout,
imports waiting on the pause, work arriving during draining, detached tasks, and
reconnection without a settings change. Retained remote chats and transfers still
need a separate drain protocol; child-overlay dependency handling, macOS mount
draining, and Windows transport also remain.

## Draining dependent Linux chat views

Idle child overlays now drain before their parents. The backend checks every open
view’s saved parent marker against its live relationship and base path before
closing any view. Missing parents, unreadable markers, inconsistent metadata, and
cycles refuse retirement. Ordering is iterative so deeply nested views do not
consume the call stack.

Each view retains the existing checked-save and ordinary-unmount behavior. A busy
child leaves its ancestors mounted. If children have drained before a busy parent
refuses, their saved changes remain available: reopening reconstructs the same
hierarchy from parent markers. Neither draining nor reopening lands child edits
into the parent or changes the project.

A mandatory Linux FUSE regression exercises three nested views, malformed parent
metadata, a busy leaf, partial draining at a busy ancestor, reopening after refusal,
and reconstruction from disk after all views drain. CI runs it alongside both
shared-backend mounted contracts. Pure ordering tests cover multiple trees and
cycles. Retained remote-work draining, macOS mount draining, and Windows transport
remain separate.

## Replacing backends with persisted remote mirrors

Inactive chat mirrors no longer block Unix replacement merely because their
routing and completed pull records remain cached. The backend verifies every
cached chat’s saved owner, its lead’s saved owner, and the lead’s mirror record.
Verification runs on the database writer queue behind earlier detached placement
writes. A failed write, missing record, or mismatched owner refuses replacement.

The checks run before disconnecting and again after receivers finish buffered
events. Active or failed pulls, transfers, runs, Autopilot drivers, forwarded
ports, queued imports, held results, cancellation state, and local Keep/recheck
decisions still block retirement. Active event tasks remain counted throughout.

No mirror data or live routing is cleared during preparation. A later local
refusal keeps the existing routing available and saved hosts reconnect normally.
A replacement loads ownership from the database and resumes synchronization from
the saved mirror state, including imported-result numbers and unreported landings.

Tests cover persisted lead and child mirrors, preservation of import and landing
records, routing after preparation, missing mirror records, active runs and
local decisions, pull states, and ownership changing while receivers drain.
Draining active remote work or persisting its remaining local decisions,
macOS mount draining, and Windows transport remain separate.

## Persisted remote decisions

Each remote chat’s Keep choice and recheck count are saved in versioned records
under the data directory’s remote-decisions folder. Hashed filenames stay within
that folder; private atomic writes and per-record locks preserve complete records.
Keep includes the exact sync state and host it applies to, so an identical replay
retains the choice and a new sync clears it.

Recheck increments are saved before requesting remote checks. A new message and
successful landing save the reset; forgetting or changing a chat’s host clears
the old choices. Keep returns a save error to its caller. Event-driven save
failures are logged and retain the live choice for a checked upgrade retry.
Startup reports unreadable, corrupt, or unsupported records rather than resetting
the budget or forgetting a choice.

Replacement checkpoints live decisions and retries failed writes before validating
mirror ownership. Tests cover restoration, replay and reset boundaries, corrupt
records, storage failure, successful retry, and forgetting a chat. Active-work
draining is the next phase.

## Active remote-work draining

With RPC admission closed, the backend pauses host maintenance and gives local
remote-work queues time to finish before taking the result-worker pause. The
existing one-second producer timeout bounds this wait. Timing out releases the
maintenance pause and leaves accepted imports, transfers, relays, and forwarded
traffic running; it never aborts them.

After acquiring the result pause, the backend checks for work that arrived while
acquiring it. If anything arrived, it releases the pause and drains again. The
existing receiver join and retained-work checks still catch events that arrive
during connection shutdown. Late work refuses retirement and resumes on the
existing owner, whose saved hosts reconnect as before.

Remote runs and Autopilot drivers belong to their server and no longer count as
local work that must end. Their chat ownership and mirror records must pass the
same persistence checks; unknown owners refuse retirement. No Stop request is
sent. A fresh connection restores runs, drivers, approvals, and questions from
server status. Failed pulls retry from their durable revision on reconnection.

Tests cover a result arriving while the pause is being acquired, work continuing
after a drain timeout, real forwarded traffic surviving timeout and completing
normally, and run/driver restoration with synchronization at the saved revision.
Long-lived forwarded pages or MCP relays, unresolved held results, and transfers
that outlast the grace period continue to defer replacement safely. macOS mount
draining and Windows transport remain separate platform work.

## macOS mount draining

The Unix backend now runs the same guarded virtual-file and child-before-parent
chat drain on Linux and macOS. On macOS it first calls `unmount` with flags zero.
Only successful detachment allows libfuse channel cleanup, listener removal, and
joining the serving session. Errors retain all live mount ownership; there is no
forced-unmount or diskutil fallback on this path. If the installed macFUSE backend
refuses ordinary unmount, replacement stays deferred rather than forcing it.

Chat stores are checked before unmount and saved again afterward to capture final
kernel writes. Busy views retain their store locks and remain writable. A partial
drain preserves already-detached descendants on disk for reopening. Persistence
errors retain the backend and attempt to restore a detached view.

The mounted dependency regression is available on Linux and macOS, including
writes after refusal and reconstruction from saved edits. Run it on a Mac with
macFUSE installed and enabled (FSKit or kernel backend, according to the system):

```sh
ROVIK_MOUNTED_BACKEND=1 cargo test --manifest-path src-tauri/Cargo.toml -p rovik-core --lib mounted_child_views_drain_without_losing_their_dependency_tree -- --ignored
```

The test requires a working mount and fails rather than skipping when unavailable.
Hosted macOS CI builds the platform path and runs the non-mounted drain tests;
it does not install or enable macFUSE. Native mounted validation is still required
for both macFUSE backends.

The **E2E** workflow’s manual `macos-mounts` platform invokes the
**macOS mounted backend** workflow and attempts this FSKit regression on a hosted macOS runner. It installs macFUSE, requests FSKit module
enablement, and runs the ignored mounted test explicitly. Installation,
enablement, or mount failure fails the run; none is treated as a skipped success.
The test log and mount/module diagnostics are uploaded as artifacts. This does
not validate the kernel backend: macOS does not support third-party kernel
extensions inside the hosted runner's virtual machine.

### Native validation findings

The [hosted FSKit run](https://github.com/piliugin-anton/rovik-ide/actions/runs/42858428090)
installed macFUSE 5.5 and enabled its FSKit module, but the test failed while
opening its first view: `fuse_chan_fd` did not supply a device descriptor.
No busy-unmount or saved-edit assertions ran in that run. The FSKit adapter now
uses MFMount's public message-channel API and a small optional custom transport
in the vendored fuser 0.18 crate (`src-tauri/crates/fuser/FORK.md`). Kernel mounts
still use device descriptors. INIT is served concurrently with mounting, split
request bodies are copied into fuser's request buffer, and scatter/gather replies
and notifications retain their message boundaries. A timed-out mount is cancelled
and cleaned up when its mount operation completes. Ordinary upgrade refusal
leaves the channel open; only successful unmount releases it.

The native workflow tests the transport and adapter before running the mounted
regression. This implementation still needs a successful native mounted run;
unit tests and Linux mounts do not establish FSKit busy-unmount behavior.

The native GUI run exposed idle RPC disconnections and unordered terminal input.
Frame deadlines now begin with the first received byte rather than socket
readiness, and frontend terminal writes are sequenced per terminal. Queued input
is rejected after an earlier write fails or the backend generation changes.
The two-window project tests passed; full isolated-chat GUI validation still
requires a working mounted backend. Native kernel-backend validation remains
unavailable on GitHub-hosted virtual machines.
