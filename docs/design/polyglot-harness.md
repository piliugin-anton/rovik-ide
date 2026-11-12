# Polyglot verification harness

The implementation follows the attached **Polyglot AI Coding-Agent Harness,
version 1.0 (November 12, 2026)**. Its Python controller is a reference technology
choice. Rovik implements the same boundaries in the `rovik-harness` Rust crate,
so the desktop, daemon and headless agent can share one controller. The existing
provider, process and permission adapters remain the integration points.

## Trust boundary

The controller owns run transitions, policy, approved contracts, command
registrations, immutable artifacts, receipts and the SQLite ledger. Models,
repository files, native tool output and candidate-generated reports cannot
grant authority. A digest identifies bytes; it does not authenticate their author.

The implementer receives an isolated workspace and scoped tools. Final checks
use a frozen candidate, protected oracle assets and fresh build directories.
No worker-writable cache, host home, credential, Git configuration or container
socket belongs inside an execution sandbox. Build scripts and compiler plugins
are executable repository code and need the same boundary as tests.

An accepted result binds source, contract, design, policy, oracle, plan,
environment, checker and configuration. Missing, skipped, unsupported, malformed
or stale required checks block acceptance. A successful exit without the
required case inventory is insufficient. Acceptance never authorizes deployment.

## Domain rules

- JSON is bounded, rejects duplicate keys at every depth, rejects unknown typed
  fields and schema versions, and uses integers or decimal strings rather than
  floating point. Canonical metadata has sorted keys and no extra whitespace.
- Scope patterns are literal relative paths or directory patterns ending in
  `/**`; `**` is an explicit whole-tree scope. Other glob syntax is rejected.
  Every changed path, including a rename's source, must pass every policy layer.
  Protected paths and Git metadata cannot become writable through narrower rules.
- Policy intersects command permissions, unions obligations and takes the
  smallest upper budget. Contradictory semantic facts block resolution.
- A template never authorizes execution. Rust, C++ and C need explicit target,
  runtime, allocation and error facts, pinned tools/dependencies, and a complete
  configuration matrix. The controller does not invent production defaults.
- A check plan must cover all mandatory obligations in every configuration.
  Behavioral checks require a positive minimum or explicit case IDs. Final
  checks cannot depend on artifacts produced by advisory checks.
- Contracts retain blocking unknowns until resolved. Human approval is an
  authenticated record bound to an exact subject; an `approved` field in model
  output is rejected. Domain/ABI/unsafe/FFI/policy changes require the appropriate
  approval before a production run can accept them.

## Delivery and evidence

Each required milestone is a separate validated commit and push. Optional M7
(extra languages, final-check caching, formal tools and parallel workers) is
outside the initial implementation.

| Milestone | Scope                                                           | Status                               |
| --------- | --------------------------------------------------------------- | ------------------------------------ |
| M0        | Contracts, profiles, scope, restrictive policy, plan validation | Implementation and negative fixtures |
| M1        | Artifact store, ledger, state machine, gate and recovery        | Pending                              |
| M2        | Isolated runner, registry, receipts, native normalizers         | Pending                              |
| M3        | Provider/broker integration and Rust quota vertical slice       | Pending                              |
| M4        | C/C++ packs, configured graphs and consumers                    | Pending                              |
| M5        | Risk obligations, failure injection and interop                 | Pending                              |
| M6        | Evaluation, accounting, approval maintenance and recovery       | Pending                              |

The supplied v2 contract and result JSON schemas are preserved in
`src-tauri/crates/rovik-harness/schemas`. The earlier blueprint's v1 schemas were
not attached; unknown versions fail closed rather than guessing a migration.
Historical legacy verification reports are not final v2 evidence.

Deployment requires an operator to review this threat model, register exact
toolchain images and dependency assets, select supported targets, and authorize
the protected oracles and approval identities. Unit tests do not replace that
deployment review or establish containment against arbitrary hostile code.
