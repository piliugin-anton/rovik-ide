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

| Milestone | Scope                                                           | Status                                  |
| --------- | --------------------------------------------------------------- | --------------------------------------- |
| M0        | Contracts, profiles, scope, restrictive policy, plan validation | Implementation and negative fixtures    |
| M1        | Artifact store, ledger, state machine, gate and recovery        | Transaction, recovery and gate fixtures |
| M2        | Isolated runner, registry, receipts, native normalizers         | Native isolation and inventory fixtures |
| M3        | Provider/broker integration and Rust quota vertical slice       | Pending                                 |
| M4        | C/C++ packs, configured graphs and consumers                    | Pending                                 |
| M5        | Risk obligations, failure injection and interop                 | Pending                                 |
| M6        | Evaluation, accounting, approval maintenance and recovery       | Pending                                 |

The supplied v2 contract and result JSON schemas are preserved in
`src-tauri/crates/rovik-harness/schemas`. The earlier blueprint's v1 schemas were
not attached; unknown versions fail closed rather than guessing a migration.
Historical legacy verification reports are not final v2 evidence.

Deployment requires an operator to review this threat model, register exact
toolchain images and dependency assets, select supported targets, and authorize
the protected oracles and approval identities. Unit tests do not replace that
deployment review or establish containment against arbitrary hostile code.

## Durable core (M1)

The store publishes synchronized blobs before registering references. Reads
verify exact length and SHA-256, and reject symlinks and hard links. SQLite uses
foreign keys, WAL, full synchronization, versioned migrations and immutable
history triggers. State updates and their events share one transaction.
Concurrent controllers must reload after a revision conflict.

Operations reserve configured budget units before execution. A duplicate active
operation key returns the same lease; different inputs cannot reuse it. Expired
leases are fenced and abandoned without promoting files left by a runner. Lost
usage retains its reservation until an operator reconciles it. Actual usage over
a reservation is recorded and prevents further spending when the account limit
is exceeded.

The pure final gate checks every evidence-key field, checker/configuration,
required case inventory, execution status, approvals and controller attestations.
It returns ready, failed or blocked; it does not authenticate receipts itself or
turn a model verdict into a state transition. M2 adds receipt admission and the
isolated execution adapter.

## Isolated execution (M2)

The initial backend requires a local Linux Docker daemon and a preloaded image
identified by its immutable configuration digest. It never pulls an image during
verification or falls back to host execution. Each check uses a fresh container,
a separate unprivileged uid, no network, no capabilities, a read-only root and
read-only source/oracle/dependency mounts. Memory (including swap), CPU, process,
build-space, output and execution-time limits are explicit registered inputs.
Setup/control operations have a separate bounded deadline for slow storage drivers.

Reports are collected inside the live sandbox because Docker's archive API does
not expose tmpfs outputs. The collector accepts only its fixed regular output
file; bounded bytes go directly to deterministic Rust, CTest/JUnit or protected
JSON inventory parsers. DTDs, inconsistent counts, missing outcomes and malformed
reports cannot become passing evidence. Whole-container termination and removal
also stop background descendants.

Receipts are created by the controller after actual execution and bind the
materialized candidate, oracle, environment, command, parameters, configuration
and active operation lease. Merely storing a worker-supplied result JSON creates
no accepted evidence. An unfinished or abandoned operation cannot supply final
results.

Patch admission checks complete trees, including deletions, rename endpoints,
case collisions and modes. Binary, executable and build-input changes require
explicit additional scope. Symlink/submodule file modes are currently unsupported
and block admission; this is not silently treated as verified support.

Native isolation regression tests are opt-in because they invoke Docker:

```sh
ROVIK_HARNESS_DOCKER=1 ROVIK_HARNESS_IMAGE=sha256:<local-image-config-digest> \
  cargo test -p rovik-harness --test execution -- --ignored --test-threads=1
```

The security fixtures were exercised with Alpine 3.22.0, local image configuration
`sha256:cea2ff433c610f5363017404ce989632e12b953114fefc6f597a58e813c15d61`.
This pins test infrastructure, not a production compiler profile. These tests
establish the documented containment properties; they do not establish protection
against an arbitrary kernel or container-runtime exploit.
