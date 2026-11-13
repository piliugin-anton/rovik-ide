# Polyglot harness integration in Rovik IDE

## Goal

Expose the existing Rust harness as one trusted IDE capability for verifying an
existing change, implementing a reviewed plan, driving Autopilot, and reporting
architecture violations in the editor. Implement and deliver the phases below
in order. Each phase includes regression tests, documentation, a commit and push.

The harness ledger is the authority for strict verification. Existing project
checks remain useful, but their ordinary success records cannot manufacture a
strict acceptance result. Models draft decisions and changes; the operator
approves exact protected inputs through trusted management actions.

Each verified task has one controller responsible for implementation and repair.
Parallel implementation workers need a separate coordinator for scopes, dependent
work and integration candidates. Their individual passing checks cannot establish
acceptance of the combined changes; the combined candidate needs verification.

## Phase 1 — Project setup and Verify changes

Create a shared core service, reachable through typed desktop and daemon RPCs.
Resolve project roots, execution hosts, ownership and private state on the trusted
backend. Negotiate the capability with connected hosts. Keep credentials and the
ledger outside candidate mounts. The current execution and local approval adapter
require Linux with a prepared Docker or rootless Podman environment.
The initial IDE entry point requires opening the Git repository root. Nested
build roots inside that repository are discovered independently; selecting only
a repository subdirectory produces actionable setup guidance.

Extend Project Checks with a verification setup panel. Discover nested manifests
and show build roots, supported profiles, missing configuration and host readiness.
Offer prepared configurations with readable scope, obligations, protected tests,
toolchain and dependency identities. Bootstrap this phase with reviewed prepared
setup bundles; phase 2 adds guided preparation without editing harness JSON.
Discovery alone does not establish an executable or approved configuration.

Capture tracked working-tree files and explicitly selected untracked files as an
immutable candidate without modifying HEAD, the index or the checkout. Report
unsaved editor buffers before capture so verification never silently checks older
disk contents. Preserve the approved baseline and protected oracle.

Source capture includes images and other binary assets, whether stored directly
in Git or hydrated by Git LFS. Capture, integrity checks, sandbox materialization
and review-bundle export stream the exact bytes; large assets are not sent as
model text. The per-file source limit is 4 GiB minus one byte, independent of the
64 MiB limit for in-memory text/control reads. Size errors identify the path,
observed size and limit. Ordinary untracked assets still need explicit selection.
Reading packed ordinary Git blobs above 64 MiB uses the local Git CLI.

Git baselines resolve LFS pointers to hash- and size-checked local objects or an
exact matching checkout. Missing objects and unhydrated working-tree pointers
produce instructions to fetch/restore them with Git LFS; inspection never runs
smudge filters or downloads files. Existing LFS paths remain pointers in prepared
Git delivery commits. Delivery writes their content to the repository's LFS store;
custom storage outside the Git directory requires applying in the IDE and committing
with Git LFS. Binary changes still require explicit scope approval, and any asset
edit invalidates the verification identity.

Verify changes runs the real controller with an initial candidate and no model
provider. It must run baseline/negative-control and final checks, retain durable
results and expose actionable Verified, Failed, Blocked and Stale states. Include
status refresh, exact approval review, cancellation and safe recovery/resume.
Changed source or configuration invalidates the visible current-result claim.

Acceptance: RPC ownership and capability checks; setup persistence; dirty and
untracked input capture; unsaved-buffer handling; genuine evidence-backed success;
failures and unsupported preparation stay blocked; stale results cannot deliver.

## Phase 2 — Verified plan implementation

Add Require verified result to Implement this plan. Bind the reviewed plan bytes
to the preparation. Guided preparation uses registered capabilities and explicit
execution inputs to produce contract/design/oracle drafts and derive the check
matrix. Show missing decisions and approvals in ordinary language. Never infer
protected test adequacy, domain semantics or execution support from filenames.

Plan and Autopilot setup reviews share the chat's available height with the
conversation and composer. Long plans and evidence scroll in a keyboard-focusable
region while the heading and Start, Cancel or Close controls stay visible.

Resolve the chosen provider, endpoint, credentials and pricing through the existing
trusted host configuration. Reuse the harness provider and durable model accounting.
The harness owns implementation and bounded repair for a verified task. Stream its
status into the existing chat; do not launch a second ordinary repair loop.

Show the candidate diff and its acceptance evidence. Preserve local edits when
reviewing or applying results. A changed destination or conflict resolution needs
a new candidate and verification. Applying a draft and delivering a verified
candidate must have distinct labels and authority checks.

Acceptance: plan identity survives preparation and retries; no provider secret in
RPC results/artifacts; configured model and budgets are used; rejection cannot be
shown as implemented-and-verified; cancellation and interrupted runs remain resumable.

## Phase 3 — Autopilot integration

Add a persisted Require verified delivery task option with a compatible default.
Reuse phase 2's service and implementation path. Keep Prepare responsible for
dependencies and Tests responsible for protected acceptance assets. Map harness
progress and blocked decisions onto existing stages rather than duplicating the
controller state machine or its repair policy.

Autopilot acceptance and delivery require current authenticated harness evidence
for the exact selected candidate. Budget exhaustion, unsupported profiles, pending
approvals, stale evidence and failed checks block verified delivery. Draft results
remain reviewable. Stopping or reopening a task must preserve run ownership and
accounting. Ordinary tasks keep their existing behavior.

Acceptance: stage transitions and resume; one implementation/repair owner; approved
configuration reused without fabricating new approval; failed/stale/missing evidence
cannot reach verified delivery; ordinary task regressions remain green.

## Phase 4 — Editor architecture feedback

Reuse the approved ownership/dependency policy and authenticated graph findings.
Surface violations with source locations, violated rules and relevant target
components in the editor/Problems UI. Feed confirmed boundaries and invariants
into verified implementation context before edits.

Associate findings with their source snapshot and configuration. After edits, mark
old diagnostics stale or remove them; never reuse them as current proof. Live or
partial analysis is advisory. Full strict verification still runs the complete
required matrix before acceptance. Respect collector precision: Cargo package
graphs, JS/TS/Python import graphs, configured native semantic graphs.

Acceptance: navigation and diagnostic lifetime; project/host isolation; stale and
cancelled analysis cannot overwrite newer results; missing rules produce setup
guidance; advisory feedback cannot grant acceptance.

## Validation and delivery

Run repository-required frontend tests, type checking and formatting before each
commit. Run affected Rust crate tests and Clippy, plus focused transport, controller
and component regressions. Use the prepared Linux environment for real isolation
tests. Validate desktop/daemon compatibility and the existing mixed Tauri roots.
After pushing the final phase, inspect the PR's CI results and fix regressions.

## Implementation record

- Phase 1: implemented. Project discovery and prepared setup import, trusted
  desktop/host routing, exact approvals, saved candidate capture and model-free
  verification are wired into Project Checks. Frontend tests, desktop tests, core
  tests, 919 protocol/client/daemon tests and a real baseline/control/final
  verification passed. The native run preserved the checkout and index and made
  no model calls. Clippy added no warnings.
- Phase 2: implemented. Guided preparation, exact saved-plan binding, trusted
  provider selection and reservation accounting, bounded private implementation,
  review/export, cancellation and recovery are connected to the existing plan
  action. All 1,156 frontend cases, type checks and formatting passed. The native
  plan test made one priced model call, verified the private candidate and
  preserved dirty source and the Git index. Full Rust validation also exposed
  independent timing and file-history races; their fixes are delivered separately.
- Phase 3: implemented. Strict task ownership,
  prepared asset stages, frozen budgets/provider prices, restart and cancellation,
  exact application approval and durable receipts are wired into Autopilot. Cloud
  delivery currently supports keeping the applied result on its execution host;
  other destinations remain awaiting delivery. All 1,172 frontend cases, types,
  formatting and affected Rust cases passed after correcting test fixtures. Two
  existing tests timed out in the broad local run and passed individually. The
  native application test required exact delivery approval, applied the accepted
  candidate and invalidated its receipt after an edit. Clippy added no warnings.
- Phase 4: implemented. Approved architecture
  rules enter implementation context, and authenticated observations feed advisory
  Problems and source-matched local markers. Applied candidates use an exact
  authenticated receipt binding. Raw filesystem changes invalidate feedback before
  asynchronous reloads; cloud findings remain historical until desktop source
  correspondence can be established. All 1,198 frontend cases and 3,856 Rust
  cases passed, including the previously timing-sensitive tests in the full run.
  Types and formatting passed, and Clippy added no warnings. The native
  application test confirmed the receipt bridge and invalidation after an edit.
