# Rovik IDE: guide for coding agents

A Tauri desktop IDE (Rust core, SolidJS UI) with a built-in AI agent and cloud hosts.
**Quality over speed.** A slower, correct, well-tested change beats a fast one that needs a
second pass. Read the code you change, its callers and its tests before editing.

## Layout

- `src/`: SolidJS frontend. `components/` (UI), `lib/` (logic, no JSX), `*.test.ts(x)`.
- `src-tauri/src/`: the Tauri app (commands). `src-tauri/crates/`: `rovik-protocol` (shared
  types, `Request`/`Response`), `rovik-core` (agent loop, tools, connectors), `rovik-client`,
  `rovik-agentd` (cloud daemon), `rovik-headless`, `eoka` (browser; has its own CLAUDE.md).
- Docs: `README.md`, `docs/cloud-hosts.md`; decision ids in comments (D6, K09…) are in `docs/design/`.

## How to write code here

- **SOLID**: one job per module and function. Add behaviour by implementing an abstraction
  (e.g. the `Connector` trait, a `ConnectorSpec` in `CONNECTOR_CATALOG`), not by adding
  kind-specific branches to generic code. Generic code depends on the trait, never on one kind.
- **DRY**: search before you write. Reuse `process.rs`, `connectors/harness.rs`, `net::client()`,
  `lib/*` helpers; when you copy code, extract it instead.
- **KISS**: the simplest design that solves the actual problem. No speculative options, unused
  parameters, or layers "for later". Delete dead code you create.
- Keep changes minimal and on topic; don't reformat or refactor unrelated code in passing.
- Errors are values with a message that says what failed and what to do; no `unwrap`/`expect`
  on IO, user or network data outside tests. Never swallow an error silently.
- Stored settings and protocol types stay compatible: new fields get serde defaults.
- Security first: secrets go through the keychain/key store, never into logs, argv or temp files
  others can read; paths stay inside the project unless the user allows otherwise.
- Comments explain intent and why, in plain sentences; every public item has a doc comment.
  Update README/docs when user-visible behaviour changes.

## Tests

- Every change ships with tests that would fail without it: unit tests beside the code, run
  e2e tests in `rovik-core/src/agent_run/*_e2e_tests.rs`, component tests in `*.test.tsx`.
- Name tests as sentences that state the behaviour.
- Never skip, disable or weaken a test to get green. Fix a flaky test by removing its race.
- Tests that call real CLIs or the network are `#[ignore]` and gated by env (`ROVIK_REAL_CLAUDE=1`).

## Before every commit (all must pass)

- `npm test`, `npm run typecheck`, `npm run format:check` (fix with `npx prettier --write <files>`).
- From `src-tauri/`: `cargo test -p <each crate you changed>`; `cargo clippy -p <crate> --tests`
  must add no warnings.
- Format Rust with `rustfmt --edition 2021 <files you changed>`, not `cargo fmt --all`: other files
  are not rustfmt-clean, and reformatting them buries your change.
- The app crate (`src-tauri/src`) needs GTK/WebKit to build; CI builds it on Linux, macOS, Windows.
- One topic per commit; the message says what changed and why.
