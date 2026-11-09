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
3. Per-client project registration, filesystem and execution ownership, including
   terminal controls. Separate
   native window callbacks from backend services.
4. Complete the subscription-dependent adapters with the event transport:
   terminal output, filesystem watches, VFS subscriptions, streamed model
   replies, host setup progress, and native callbacks. Scope subscriptions to
   clients and clean them up on disconnect without stopping shared work.

Each phase must pass its regression tests and the repository checks before
commit and push. Event-dependent commands must not silently lose callbacks or
pretend they are ready for IPC. The UI remains embedded until these are complete.

## Following this migration

Automatic daemon packaging/start/attach; per-window event routing and reconnect
recovery; lifecycle, crash, and upgrade behavior; Windows named pipes; real
multi-window FUSE and execution isolation tests.

## Private event transport prerequisite

VFS subscriptions now use client sessions and a separate authenticated event
connection, so output does not wait behind an RPC call. Embedded clients use
the same resource ownership and callback routing. Disconnect and callback-loss
cleanup cannot cancel another client's resources. Queues are bounded by event
count and bytes; overflow fails the session explicitly. This is the foundation
for phases 3 and 4, not global agent event replay or reconnect recovery.
