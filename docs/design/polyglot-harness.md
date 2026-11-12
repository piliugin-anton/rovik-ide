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

| Milestone | Scope                                                           | Status                                                  |
| --------- | --------------------------------------------------------------- | ------------------------------------------------------- |
| M0        | Contracts, profiles, scope, restrictive policy, plan validation | Implementation and negative fixtures                    |
| M1        | Artifact store, ledger, state machine, gate and recovery        | Transaction, recovery and gate fixtures                 |
| M2        | Isolated runner, registry, receipts, native normalizers         | Native isolation and inventory fixtures                 |
| M3        | Provider/broker integration and Rust quota vertical slice       | Native patch, approval and provider fixtures            |
| M4        | C/C++ packs, configured graphs and consumers                    | Protocol, configured graph and native consumer fixtures |
| M5        | Risk obligations, failure injection and interop                 | Native sanitizer, failure and ABI fixtures              |
| M6        | Evaluation, accounting, approval maintenance and recovery       | Pending                                                 |

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

## First vertical slice (M3)

`rovik-core::harness::HarnessProvider` uses Rovik's shared HTTP client and stream
parser with exactly one request per controller reservation. Credentials stay in
the trusted host adapter. Explicit pricing and reported token usage determine
cost; absent usage retains the reservation and blocks further advancement. One
bounded schema repair is permitted. Tool requests and patches pass strict JSON
validation; provider-native tools cannot execute.

The broker reads immutable source, retrieves literal matches and lexical symbol
locations, exposes admitted diagnostics, and requests only registered advisory
checks. Proposals must map every invariant and pass complete-tree admission.
Context retains exact mandatory policy, contract, design and target profiles;
repository text remains attributed data. Insufficient context space blocks the
task rather than dropping mandatory constraints.

The controller advances every mandatory state. Before implementation it executes
a relevant negative control and requires the expected behavioral case to fail.
Compilation failure does not validate an oracle. Final checks run on a new clean
container with the admitted patch. The gate checks current approvals and the
repository revision again immediately before acceptance.

The reference Rust fixture uses the supplied quota API and seven boundary cases,
including zero and the full unsigned 64-bit range. A separate protected consumer
uses the public API; implementation-local tests cannot replace it. Its toolchain
and target are deliberately narrow: Rust 1.85.1, edition 2021, Linux x86_64,
standard library and no external crates. These illustrative facts never fill
unknown production profile fields.

Build the operator CLI with `cargo build -p rovik-headless --bin rovik-harness`.
Invoke `rovik-harness` without arguments for its command syntax. Keep its absolute
state directory outside the repository. Set `ROVIK_HARNESS_DOCKER_BIN` when the
Docker client is installed outside `/usr/bin/docker`. `import` captures tracked HEAD bytes
without changing the worktree; `put` registers immutable assets; `create` checks a
complete `TaskSetup`. Review the emitted exact subject, scope and policy digests,
then use `approve` with an `ApprovalRequest` JSON file. Approval authenticates the
local Linux UID against the setup's operator authority registration. Repository
files and model output cannot invoke this management interface.

`run` uses an explicitly selected repository, model endpoint and rates file;
`ROVIK_HARNESS_API_KEY` supplies the credential. Rates contain `input`,
`cached_input` and `output` microunits per million tokens. `status`, `verify` and
`export` expose the durable result and immutable candidate. Export requires a new
directory. Acceptance and export do not apply a patch, commit, merge or deploy.
Existing IDE chat verification retains its current behavior; this is the opt-in
strict workflow shared by the core and the new headless entry point.

The native test image can be built from
`src-tauri/crates/rovik-harness/fixtures/native/Dockerfile`. Resolve the resulting
local image configuration digest with `docker image inspect --format '{{.Id}}'`
and pass it as `ROVIK_HARNESS_IMAGE`; the harness never authorizes a mutable tag.
Package installation happens while preparing that image, outside verification.
Run the Rust vertical test with `ROVIK_HARNESS_DOCKER=1` and
`cargo test -p rovik-harness --test controller -- --ignored`. The test uses a
scripted provider to propose a real source change and actual native compiler and
consumer execution; it makes no claim about model quality or production approval.
Backend v2 permits execution in the size-limited build mount so compiled consumers
can run, while temporary and report mounts remain non-executable.

## Native packs and supported matrix (M4)

The controller remains Rust. A protected Python subprocess adapts Cargo and CMake
inside the existing offline sandbox. It consumes one bounded JSON request and
returns one correlated protocol-v1 response. Probe, index, proposed-plan and
explanation operations share this boundary. Proposed plans and explanations are
advisory; only configured indexing can produce a static graph result, and the
Rust core independently checks its precision, configuration, references, required
test discovery, flags and forbidden transitive edges before admitting evidence.
Requests live in the approved oracle, selected by registered request IDs.

| Pack | Tested configuration                                                | Evidence                                                                                                                                 |
| ---- | ------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| Rust | 1.85.1, edition 2021, x86_64-unknown-linux-gnu, std                 | Full offline locked Cargo resolve graph with features/target filtering; protected quota consumer with unsafe forbidden                   |
| C++  | GCC 12/libstdc++, C++23, x86_64-linux-gnu, exceptions/RTTI disabled | Fresh CMake codemodel and compiler-derived include graph; separately compiled public header consumer                                     |
| C    | GCC 12, C17, x86_64-linux-gnu, hosted                               | Fresh compile database, codemodel and native CTest discovery; separate implementation/consumer compilation, null and failure-state cases |

Each feature configuration is checked separately. Cargo discovery without a
resolve graph is insufficient. CMake always configures a fresh build directory;
a preexisting compilation database is rejected. Required consumer commands cannot
be omitted from any configuration. Missing tools, other compilers, targets,
freestanding runtimes and unresolved profiles block rather than falling back to
lexical assurance. C/C++ graph support currently uses GCC dependency output;
Clang, cross-target execution and mixed-language compilation databases need their
own conformance configuration before they are supported.

Run `cargo test -p rovik-harness --test controller --test packs -- --ignored
--test-threads=1` with the native Docker opt-in and pinned image environment above.
These tests exercise all three public APIs, a feature-gated forbidden Cargo edge,
and configured C/C++ include edges. Standard tests cover protocol corruption,
missing required discovery (including doctest obligations), insufficient graph
precision, transitive boundary violations and omitted consumers. Declaring a
required doctest still requires its separate registered behavioral command; a
Cargo dependency graph alone never establishes doctest execution.

## Risk and interop (M5)

The core unions declared risks with signals from both sides of the immutable
source diff. Removing allocation cleanup, introducing unsafe/FFI code, or changing
recognized public declarations cannot be hidden by worker-provided tags. This is
a conservative lexical supplement to the approved design and profile obligations;
it is not a proof that undetected risks are absent. The assessment is persisted
and independently recomputed at final verification.

Nonempty `risk_policy.routes` must be hashed into
`policy.facts.risk_policy_digest`, so approvals and evidence bind the routing
rules. Each applicable route requires registered behavioral commands in every
final configuration and authenticated review of the exact candidate. Advisory
success does not discharge a risk. New concurrency signals currently block: this
release has no conformant race/linearizability runner, and sequential consumers
cannot supply that assurance.

The interop bank separately compiles a C producer and actual C, C++ and Rust
consumers. It checks C/Rust struct size and field offset, returned values/status,
caller ownership, one-time release, borrowed callback lifetime, and unchanged
output on errors. Controlled allocation failures at every acquisition point check
partial cleanup. C and C++ modes compile and link with AddressSanitizer and
UndefinedBehaviorSanitizer, verify instrumentation symbols, and run with error
recovery disabled. Rust exercises ABI/lifetime behavior separately; it is not
represented as a Rust sanitizer run.

Protected mutants introduce cleanup leaks, forbidden output mutation, retained
callbacks, ABI behavior changes, buffer overruns and signed overflow. Correct
consumers must pass and mutants must fail after successful compilation. Removing
sanitizer flags is rejected from effective compiler arguments and from actual
object instrumentation. Run the native bank with the pinned image and
`cargo test -p rovik-harness --test risk -- --ignored`; its twenty observations
are regression evidence for this documented configuration, not arbitrary FFI or
threading support.
