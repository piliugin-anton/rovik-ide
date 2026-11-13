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

Each required milestone is a separate validated commit and push. The v1 baseline
also includes the Rust sandbox tools and JavaScript, TypeScript and Python packs.
Final-check caching, formal tools and parallel workers remain outside this scope.

| Milestone | Scope                                                           | Status                                                  |
| --------- | --------------------------------------------------------------- | ------------------------------------------------------- |
| M0        | Contracts, profiles, scope, restrictive policy, plan validation | Implementation and negative fixtures                    |
| M1        | Artifact store, ledger, state machine, gate and recovery        | Transaction, recovery and gate fixtures                 |
| M2        | Isolated runner, registry, receipts, native normalizers         | Native isolation and inventory fixtures                 |
| M3        | Provider/broker integration and Rust quota vertical slice       | Native patch, approval and provider fixtures            |
| M4        | C/C++ packs, configured graphs and consumers                    | Protocol, configured graph and native consumer fixtures |
| M5        | Risk obligations, failure injection and interop                 | Native sanitizer, failure and ABI fixtures              |
| M6        | Evaluation, accounting, approval maintenance and recovery       | Repeated native evaluation and process-loss rehearsal   |

The complete contract and result formats are published as Rovik protocol v1 in
`src-tauri/crates/rovik-harness/schemas`, with the corresponding v1 example.
The unreleased development revisions are consolidated into this first baseline:
contracts, results, pack messages, runner/checker identities and the ledger all
use version 1. Unknown versions fail closed. Existing IDE verification reports
are not authenticated strict-harness evidence.

Deployment requires an operator to review this threat model, register exact
toolchain images and dependency assets, select supported targets, and authorize
the protected oracles and approval identities. Unit tests do not replace that
deployment review or establish containment against arbitrary hostile code.

## Durable core (M1)

The store publishes synchronized blobs before registering references. Reads
verify exact length and SHA-256, and reject symlinks and hard links. SQLite uses
foreign keys, WAL, full synchronization, one atomic initial migration and immutable
history triggers. State updates and their events share one transaction.
Concurrent controllers must reload after a revision conflict.

Storage paths support Windows drive prefixes and macOS's root-owned `/var` and
`/tmp` aliases. The store retains the checked target path; other directory symlinks
remain forbidden. Fixtures can be inspected on macOS and Windows without an
approval identity. Local approval authentication and container execution still
require the supported Linux host.

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

The Docker backend requires a local Linux Docker daemon and a preloaded image
identified by its immutable configuration digest. It never pulls an image during
verification or falls back to host execution. Each check uses a fresh container,
a separate unprivileged uid, no network, no capabilities, a read-only root and
read-only source/oracle/dependency mounts. Memory (including swap), CPU, process,
build-space, output and execution-time limits are explicit registered inputs.
Setup/control operations have a separate bounded deadline for slow storage drivers.

Rootless Podman is also supported on Linux with the distinct environment identity
`rovik-podman-v1`. Select it with `ROVIK_HARNESS_ENGINE=podman`; optionally set
`ROVIK_HARNESS_PODMAN_BIN` to its absolute executable path. Docker remains the
default (`ROVIK_HARNESS_ENGINE=docker`, identity `rovik-docker-v1`). A run cannot
switch engines after approval, including during recovery.

Podman uses a private store under `<state-dir>/podman`, local execution only,
explicit user mappings and cgroup v2. It requires subordinate UID/GID ranges,
the distro's rootless Podman prerequisites and init binary (`catatonit`), and
delegated memory, CPU and pids controllers. Before any check starts, the runner
reads the container's actual cgroup limits and rejects missing or unenforced
limits. Disabled cgroups are unsupported. The cloud project-services store,
which deliberately shares per-user limits, is separate from this verification
store. Cloud daemon routing still needs the IDE integration described below;
merely having project services available does not establish strict readiness.

Prepare images separately, then import an archive into the selected engine with
`rovik-harness <absolute-state-dir> load-image <archive.tar>`. The task environment
must still pin the resulting image configuration digest; loading a tag does not
authorize it. Both engines use the same protected mounts, offline execution,
bounded output, whole-container cleanup and authenticated receipt admission.

The isolation tests can exercise either engine. For Podman, set
`ROVIK_HARNESS_PODMAN=1`, `ROVIK_HARNESS_PODMAN_STATE` to a private prepared Podman
directory and `ROVIK_HARNESS_IMAGE` to its loaded immutable image ID, then run
`cargo test -p rovik-harness --test execution -- --ignored --test-threads=1`.

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
`cached_input` and `output` microunits per million tokens. `status`, `gate` and
`export` inspect the durable result and immutable candidate; `verify` runs a fresh
final verification attempt. Export requires a new
absolute directory and writes `patch.json`, `candidate.json`, `setup.json`,
`evidence.json`, referenced blobs and a hash manifest. The patch includes exact
preimages, additions, deletions, modes and binary identities; applying it in memory
must reproduce the complete candidate. Evidence includes failed attempts,
infrastructure recoveries, approvals and scoped exceptions. The manifest records
the run state and labels explicitly accepted residual risk. Acceptance and export
do not apply a patch to the user's checkout, commit, merge or deploy.

`run` exits with `0` for accepted, `2` for blocked, `3` for stopped/rejected,
`4` for invalid input or policy, `5` for cancelled, and `1` for unexpected
controller failure. Management commands return zero when their action completes;
that is not an acceptance verdict. Errors include a machine-readable category.
Existing IDE chat verification retains its current behavior; this is the opt-in
strict workflow shared by the core and the new headless entry point.

Build the statically linked Rust sandbox tool with `bash scripts/build-harness-tool.sh`
on x86_64 Linux (Rust and a C compiler with static libc are required). Then build
the native test image from
`src-tauri/crates/rovik-harness/fixtures/native/Dockerfile`. Resolve the resulting
local image configuration digest with `docker image inspect --format '{{.Id}}'`
and pass it as `ROVIK_HARNESS_IMAGE`; the harness never authorizes a mutable tag.
Package installation happens while preparing that image, outside verification.
Run the Rust vertical test with `ROVIK_HARNESS_DOCKER=1` and
`cargo test -p rovik-harness --test controller -- --ignored`. The test uses a
scripted provider to propose a real source change and actual native compiler and
consumer execution; it makes no claim about model quality or production approval.
The v1 backend permits execution in the size-limited build mount so compiled consumers
can run, while temporary and report mounts remain non-executable.

## Local Docker installation

The desktop now checks local Docker at startup and automatically installs it
when missing on supported platforms. Installation uses the shared backend job
lifetime and private progress channel used by the Chromium installer; closing a
window does not cancel accepted work or allow a competing backend to replace it.
A process lock serializes installation across backends. Projects and verification
commands cannot invoke the installer.

Linux installation uses the distribution's signed packages on Debian/Ubuntu and
derivatives, Fedora, Arch/Manjaro, openSUSE and Alpine. Non-root desktops need
`pkexec` and their normal PolicyKit authentication agent. The system installer
starts the daemon and adds the current account to the Docker group; a new login
is required for that group membership. Other distributions receive an explicit
manual-installation message. No downloaded shell script is run as root.

macOS and Windows on x86_64 or ARM64 download Docker Desktop from
`desktop.docker.com`, verify its published SHA-256 and run the native installer.
Rovik does not accept Docker's agreements on the user's behalf. System approval,
first-use setup or a restart can still be necessary. Existing installations are
never replaced merely because their daemon is stopped or inaccessible.

Readiness uses explicit local endpoints and checks for a running Linux container
daemon. An installed executable alone is insufficient. This installs the local
prerequisite only: the strict runner still executes on Linux, approved toolchain
images and dependency assets still need preparation, and macOS/Windows desktops
need a supported Linux execution host for the current strict harness. Cloud
hosts retain their rootless Podman setup.

## Proposed IDE integration

This section is a recommendation; the current entry point is still the headless
CLI. The best first use case is an approved plan for a bug fix or API refactor
with explicit behavioral acceptance criteria. Add **Require verified result** to
the existing **Implement this plan** action and Autopilot task options. The IDE
should guide setup from project manifests and present missing decisions directly,
without requiring users to author harness JSON.

On first use for a project, select the language profiles, exact tools, dependency
locks, protected test commands and a Linux execution host. Reuse that approved
configuration for later tasks while its inputs remain current. The existing
Prepare stage prepares and freezes dependencies outside verification; the Tests
stage creates the protected oracle from the approved contract and establishes a
relevant failing negative control before implementation. Missing inputs or an
unsupported runtime leave the task blocked with a specific next action.

The flow should fit the existing Autopilot stages:

| Stage               | Proposed strict-mode behavior                                                                                                             |
| ------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| Plan / Plan review  | Draft the contract, scope, invariants and required configurations; accepting the plan records the corresponding approval.                 |
| Prepare / Tests     | Pin the environment and dependencies, review and freeze acceptance tests, and prove the negative control fails for the intended behavior. |
| Implement / Review  | Let the Rust controller own scoped edits, repairs, budgets and required reviews in an isolated candidate workspace.                       |
| Checks / Acceptance | Run protected checks against the frozen candidate and expose the controller's actual verdict and evidence.                                |
| Deliver             | Apply or publish the exact accepted candidate after checking the destination revision is still current.                                   |

The controller should own the strict task's implementation and repair loop so
Autopilot does not start a competing repair loop. Its progress, missing approvals,
failure reasons and case results can feed the existing stage strip and
`VerificationRecord` UI, with **Review**, **Resume** and **Cancel** actions. Model
acceptance text and ordinary check summaries cannot supply the trusted verdict.

Use the shared `rovik-harness` library through trusted core service methods and
typed desktop/daemon RPCs. Keep the ledger outside the worker workspace; the
frontend holds run IDs and renders records. The current integration points are
`PlanImplementationAction.tsx`, the Autopilot stage/driver modules,
`agent_run/verify.rs` and the delivery/overlay path. `rovik-core/src/harness.rs`
already supplies provider and Git adapters, but UI service routing and task
construction still need implementation.

The first release of this IDE mode should require isolation and use the existing
Linux Docker or rootless Podman backend, locally or on a connected Linux host.
Check the selected host's per-job isolation and resource capabilities before
offering strict mode. Capture the actual
intended starting tree: the current Git importer captures committed HEAD only,
so unsaved buffers and uncommitted edits need an explicit saved snapshot before
execution. Before applying a result, check both the destination revision and
local edits. Conflict resolution or subsequent edits produce a new candidate
that needs verification again. A blocked or failed run can remain available as a
draft diff, while verified delivery requires the accepted candidate's evidence.

## Native packs and supported matrix (M4)

The controller and sandbox adapters are Rust. The image supplies
`/usr/local/bin/rovik-harness-tool`, which adapts Cargo and CMake
inside the existing offline sandbox. It consumes one bounded JSON request and
returns one correlated protocol-v1 response. Probe, index, plan derivation, native workspace checks and explanation operations
share this boundary. Plans derive from registered capabilities and obligations;
explanations remain advisory. The Rust core independently validates graph precision,
configuration, references, test inventory, compiler flags and boundary findings
before admitting evidence.
Requests live in the approved oracle, selected by registered request IDs.
The same executable runs protected C/C++ consumers, interop regressions and the
native evaluation bank. No Python orchestration or separate deployed service is
required. Its bytes are bound by the approved toolchain image digest; changing the
tool requires a rebuilt image and new approval. Compiler subprocesses have bounded
output and deadlines, including descendants holding inherited output pipes open.

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
lexical assurance. C/C++ graphs combine configured GCC dependency output with
optional pinned libclang semantic collection. Clang as the build compiler,
cross-target execution and mixed-language compilation databases need their own
conformance configuration before they are supported.

Run `cargo test -p rovik-harness --test controller --test packs -- --ignored
--test-threads=1` with the native Docker opt-in and pinned image environment above.
These tests exercise all three public APIs, a feature-gated forbidden Cargo edge,
and configured C/C++ include edges. Standard tests cover protocol corruption,
missing required discovery (including doctest obligations), insufficient graph
precision, transitive boundary violations and omitted consumers. Declaring a
required doctest uses the registered `CargoDocTests` behavioral command; a Cargo
dependency graph alone never establishes doctest execution. `CargoTests` inventories
each compiled workspace test binary independently. `CargoFmt`, `CargoCheck`,
`CargoClippy`, `ClangTidy` and `Ctest` provide the other configured workspace checks.

### JavaScript, TypeScript and Python

The same Rust executable also handles protocol-v1 `test` requests. Register a
behavioral command with `packs::runtime_test_command`, add its request to the
protected oracle, and include the command in the profile's public consumers and
every required final configuration. `runtime::TestInput` binds the complete
profile, configuration, protected test files and required case IDs. TypeScript
also requires a protected compiler project. `fixtures::quota_runtime` provides
unapproved illustrative setups for all three languages.

| Language   | Tested toolchain                          | Verification                                                                                                     |
| ---------- | ----------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| JavaScript | Node.js 24.19.0, npm 11.17.0, ESM         | Offline npm installation and protected `node:test` consumers                                                     |
| TypeScript | TypeScript 7.0.2 with the same Node image | Strict compilation with emit blocked on errors, followed by protected Node consumers of the generated JavaScript |
| Python     | CPython 3.11.2, pytest 7.2.1              | Hashed offline wheel installation, syntax compilation and isolated pytest consumers                              |

Runtime versions are exact profile facts and checked against installed tools.
Node projects need `package.json` with `type: module` and `package-lock.json` v3.
Dependencies need integrity hashes; registry packages use a prepared npm cache
under `/dependencies/npm`, while pinned local archives may live under
`/dependencies/packages`. Verification runs `npm ci --offline --ignore-scripts`.
Workspace links and dependency install scripts are not supported by this profile.

Python projects need `requirements.lock`: one exact `name==version` requirement
per line with one or more `--hash=sha256:...` values; comments and an empty lock
are allowed. Include every transitive dependency and its wheel under
`/dependencies/wheels`. Installation uses `--no-index --require-hashes
--only-binary=:all:` into an isolated build directory. Source distributions,
editable installs and requirement-file options are rejected. Pytest starts with
Python's isolated mode, disabled plugin autoload and no repository conftest or
pytest configuration; the Rust driver selects the project and installed-package
import roots after startup. Python is needed to verify Python code, with all
harness orchestration remaining Rust.

Native Node and pytest reports are independently parsed by the controller.
Malformed or truncated reports, missing required cases, zero executed tests,
tool-version mismatches, dependency failures and TypeScript errors cannot count
as a successful check or a behavioral negative control. Skipped required cases
block acceptance. Test names must be unique within each check. Browser/Deno/Bun
runners, Jest/Vitest integrations and Python type checking still need separately
registered and validated checks.

#### Architectural boundaries for runtime languages

JavaScript, TypeScript and Python support protected `index` requests through
`packs::graph_command`. Register the request in the oracle and include that command
as a required final check in every applicable configuration. Use the same resolved
runtime profile and offline dependency bundle as the behavioral checks. TypeScript
index requests also name a self-contained protected `typescript_project` JSON file
covering the analyzed modules; repository compiler configuration cannot replace it.

The Rust adapter parses every project module with pinned language grammars and
returns `project_imports` evidence. Node resolves ESM imports and CommonJS literal
`require` calls without executing the imported code; the pinned TypeScript compiler's
resolution trace covers type imports, re-exports, extension substitution and protected
path aliases. The adapter also parses emitted JavaScript and follows Node resolution
from its emitted location, so conditional exports cannot hide a forbidden runtime
dependency behind a different type declaration. Python resolves absolute and relative
imports against the prepared project, wheel and interpreter search roots without
importing candidate packages. Namespace packages and package initializers contribute
dependencies too. Imports in conditional branches are conservatively included.

Rules use exact node IDs or directory patterns ending in `/**`. For example, this
part of an approved index request prohibits dependencies from any domain module
into infrastructure, including indirect paths through shared modules:

```json
{
  "forbidden_edges": [{ "source": "domain/**", "target": "infrastructure/**" }]
}
```

Project node IDs are repository-relative paths. Imported external modules have
IDs such as `node:fs`, `npm:fixture-identity` and `python:sqlite3`. Those modules are
opaque leaves: the check covers project import paths, including paths ending at an
external module, and does not claim to analyze third-party internals. It does not
prove all runtime dependencies or DDD semantics through arbitrary reflection.

Missing modules, invalid syntax, insufficient graph precision, missing source
layers, tool-version mismatches and unsupported loading block the check. Computed
JavaScript imports, aliased `require`, escaped specifiers, TypeScript reference
directives, Python wildcard imports and dynamic Python import machinery/reflection
(including `importlib`, `runpy`, `builtins` and import-related `sys` mutation) require a separate
checker. Python project modules cannot shadow standard-library names. The current
Python search layout is the project root followed by prepared wheels and
interpreter roots; custom `sys.path` and zip import roots are unsupported.

The controller independently validates the observed graph and rejects direct or
transitive forbidden paths. A graph does not count as behavioral test evidence.
Native conformance tests exercise these checks with `cargo test -p rovik-harness
--test imports -- --ignored`, using the rebuilt pinned image and Docker opt-in
described above. Rebuilding the tool changes the approved image identity; existing
image approvals do not silently acquire new checker behavior.

For a mixed project such as Tauri with a Rust backend and React/TypeScript frontend,
one task can require checks for both profiles against the same candidate. The
current adapters do not automatically configure Tauri: the Cargo graph expects a
root manifest or workspace, and frontend resolution follows Node/TypeScript rules,
not arbitrary Vite plugins. A nested `src-tauri` manifest, bundler-specific aliases,
assets and browser tests need registered checks with the correct working directories
and prepared dependencies. Register integration tests for command names, serialized
arguments/results and errors across `invoke()`; separate language import graphs do
not establish that IPC contract. Full Linux Tauri builds also need the appropriate
GTK/WebKit development packages, and platform packaging needs checks on its target OS.

Risk attribution distinguishes managed constructors from C++ ownership. It still
routes changed file handles, FFI and worker/thread signals to their required
evidence and reviews, inspecting removed code as well as additions. This remains
a conservative lexical supplement, not proof that a program has no other risks.

All six languages use the Linux container backend, immutable input mounts,
network denial, resource limits, approvals and evidence bindings. The feature is
enabled by creating and approving a strict harness task with the corresponding
profile and registry; there is no new Autopilot toggle or replacement for its
prepare stage. It runs on the machine executing the headless harness and requires
no separately deployed controller service.

Build the tool and image as described above, then run
`cargo test -p rovik-harness --test runtimes -- --ignored --test-threads=1`
with `ROVIK_HARNESS_DOCKER=1` and the pinned image digest. The fixtures execute
real offline npm packages and Python wheels, reject behavioral mutants, accept
repairs through the full controller, and reject compiler, runtime-version and
dependency corruption failures. The original 108-observation native evaluation
bank remains scoped to Rust, C and C++; these runtime conformance tests are
separate evidence.

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
success does not discharge a risk. Concurrency routes require registered behavioral
cases for schedules, linearizability, cancellation and shutdown in every configured
target, plus candidate review. The bounded Python fixture detects a lockless lost
update and exercises cancellation/joining. Missing suites block; these schedules do
not establish freedom from all races or provide an unconfigured ThreadSanitizer run.

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

## Operations and recovery (M6)

The v1 backend records each job and its private workspace durably before container
creation. The single `001_initial.sql` migration creates the complete v1 ledger,
including artifact reference types, job tracking, immutable actual-usage receipts,
operator reconciliations, infrastructure recovery links and append-only failure resolutions. Initialization and
the schema-version marker commit together; reopening a current ledger preserves
its history. Unreleased development ledgers from before this consolidation need a
new state directory; no compatibility upgrade or automatic data reset is performed.
Unknown schema versions block. Keep the ledger and its artifact directory together
when backing up controller state.

The controller watches the overall deadline, Ctrl-C and durable operator state
while provider or native work is running. Cancellation terminates the whole native
job; stopped, rejected and cancelled runs cannot accept or resume. Before final
acceptance, the gate, approval validity and candidate revision are rechecked under
the same SQLite write transaction that commits acceptance. Repository integration
still needs its own revision check when an operator later applies or merges an
exported candidate.

Operator commands use the same absolute state directory as `run`:

| Command                                   | Effect                                                                                                     |
| ----------------------------------------- | ---------------------------------------------------------------------------------------------------------- |
| `status <run>`                            | State, restart stage, exact binding, operations and reserved/actual budget units                           |
| `approvals <run>`                         | Exact missing/expired/revoked approval requirements and approval history                                   |
| `revoke <run> <approval-id> <reason>`     | Append an authenticated revocation for that run                                                            |
| `cancel <run>`                            | Commit cancellation, fence work and remove registered native jobs                                          |
| `recover <run>`                           | Block interrupted work, fence old callbacks, remove registered jobs/workspaces and verify stored artifacts |
| `reconcile <run> <usage.json>`            | Record actual usage for an abandoned operation and release its reservation                                 |
| `resolve-failure <run> <resolution.json>` | Review a historical result against an exact current passing replacement                                    |
| `resume <run>`                            | Resume the recorded restart stage after prerequisites and accounting are repaired                          |
| `retention <grace-seconds>`               | Preview aged unregistered blobs; append `--apply` to delete them                                           |

The immutable intake's authority must grant `operate` for cancel/recover/resume,
`accounting` for reconciliation and `failure-resolution` for failure disposition.
Approval grants and revocations retain their subject-specific authority checks.
These are trusted management capabilities, unavailable through the worker broker.
Retention requires access to the private controller store and never runs as a
worker tool.

After controller loss, run `status`, then `recover`. Recovery also rechecks earlier
job identities, since a daemon may finish a create request after its client loses
the connection. If the daemon cannot confirm cleanup, recovery reports an error;
restore its availability and repeat recovery. No leftover build output is admitted.
Unsettled work keeps its reservation, even when the process is gone. A
reconciliation file supplies every reserved unit and an explanation, for example:

```json
{
  "operation_id": "exact-abandoned-operation-id",
  "usage": { "model_calls": 1, "tokens": 1370, "cost_microunits": 42 },
  "reason": "Actual usage from the provider receipt for this interrupted request"
}
```

Use observed usage; the numbers above are illustrative. Unknown usage continues
to block. Actual cost above a reservation is retained, and exceeding a task limit
stops the run. A successful reconciliation does not turn interrupted output into
evidence. Resume reuses only authenticated, completed checks for the exact same
inputs; reconciled interrupted checks need a new attempt. This is same-run
idempotence, not cross-run final-check caching.

An explicitly configured `budgets.repairs` account enables a bounded implementation
repair after a compilation, behavioral or architecture-boundary failure. Each attempt consumes repair and
model budgets. Missing repair policy blocks; exhausted repair budget stops.
Protected compiler stages have distinct failure markers. A compiler failure
contains no executed behavioral cases and cannot validate a negative control.
Missing tools, wrong tool versions, broken dependency preparation and malformed
reports remain distinct from implementation failures.

Completed infrastructure failures without failed behavioral cases can retry up to
`worker.infrastructure_retries_per_check` times (default one, at most three),
within the existing check budget and deadline. An authenticated pass for the same
source, check, configuration and complete evidence binding creates an immutable
recovery link; the original observation stays unchanged. Interrupted operations
with unknown execution or usage still require recovery and reconciliation.
Infrastructure failures do not trigger code repair. The repaired candidate goes
through scope, risk, review and fresh final checks again. Earlier failures remain
visible and block acceptance until an authorized operator supplies a resolution:

```json
{
  "previous_result_id": "historical-failing-result-id",
  "replacement_result_id": "current-passing-result-id",
  "reason": "Reviewed the defect and its fix; the replacement covers the same obligation"
}
```

The replacement must be authenticated current evidence for the same check and
configuration. Revoking this approval blocks verification again. Semantic
replanning stages a new intake for explicit approval. An authorized `replans` account bounds activations; a replacement retains accounting and invalidates the selected evidence binding.
One output-format repair is the maximum supported provider repair policy, and
every provider request has its own actual-usage accounting.

`TaskSetup.exceptions` registers exact, expiring policy dispositions. Each record
names its obligation, kind, owner, rationale, task scope, affected configurations,
supporting evidence and any residual risk. The digest of the entire list must
match `policy.facts.exceptions_digest`. A matching plan disposition and a
`policy-exception` approval for the exact record are required. Coverage applies
only to the listed configurations; approval cannot implicitly waive the rest of
the matrix. Expiry and revocation are checked again at acceptance. `TRUST-001`
and `SPEC-001` cannot be waived, and exceptions never turn a failed observation
into a passing result.

Retention initially preserves every registered blob, run, approval and historical
result. It removes only unregistered CAS blobs older than an explicit grace period
of at least one day. Publication/registration and garbage collection share the
SQLite writer lock, including when an older orphan is reused, so collection cannot
race a new reference. Build workspaces are disposable after registered jobs stop.

The recovery fixture SIGKILLs a child controller during an actual Alpine native
job, verifies that the container survives the controller, then reopens the ledger
and exercises cleanup, fencing, retained reservations, reconciliation and resume.
It is part of `cargo test -p rovik-harness --test operations -- --ignored` with
the Alpine image and Docker opt-in used for isolation tests. Other regressions
cover zero-cost abandoned work, deadline/cancellation before dispatch, repair
reopen/resume, revocation, retention, and concurrent writers at acceptance.

## Evaluation and measured limits (M6)

The versioned `native-contracts-heldout-v1` bank is independent of the quota
demonstration. It contains correct and defective implementations of checked
addition, decimal parsing and bounded copying in each language. Two fresh native
repetitions exercise compiler-only, public-example-only and protected-contract
strategies: 18 variants × 2 repetitions × 3 strategies = 108 observations.
The report binds the bank, oracle, environment and authenticated native result.
Missing/duplicate trials, stale inputs and successful zero-case reports fail
validation. Infrastructure errors and unsupported configurations remain separate
from behavioral rejections.

The [recorded evaluation](../evaluation/native-verifier-v1.json) contains every
observation and measured duration. With the documented native image, each language
had six defective trials per strategy: compilation and the happy-path example
each accepted all six; protected checks rejected all six. All six correct trials
per language/strategy passed. There were zero infrastructure errors and zero
unsupported observations in this configured bank. These deliberately selected
mutants establish regression sensitivity; two deterministic repetitions do not
estimate a production defect rate or statistical confidence about model quality.

There were **zero model calls** in this measurement. The specification's three-way
live-model comparison (existing CI, scoped guidance, and enforced verification)
still requires a selected model, explicit token/cost budgets, representative
repositories and independent ground truth. Model quality, human review effort,
sequential extension effort, monetary cost per successful task, and production
latency are unmeasured. The initial native bank does not claim the full workload
coverage proposed by that study. The live-provider runner described below supplies
the comparison workflow, while the recorded measurements remain native-only.
No prompts, policies or permanent guidance are automatically promoted from a run.

Run the native evaluation with `cargo test -p rovik-harness --test evaluation --
--ignored` and the protected native image. Set `ROVIK_HARNESS_EVALUATION_OUTPUT` to
save its JSON report. `.github/workflows/polyglot-harness.yml` runs the ordinary
core tests, all supported native suites, the repeated evaluation, isolation and
process-loss rehearsal, and uploads the evaluation report.

## Preparation, planning and reviewed replanning

A complete approved `TaskSetup` can still be imported directly. `prepare` now starts
from a request, immutable repository snapshot, registered semantic vocabulary,
model limits and budgets. `draft` invokes a separate domain analyst, architect,
oracle author or reviewer with a closed response schema. Every attempt, including
format repair, uses the shared provider and durable accounting. Drafts have no
approval, shell or candidate-edit capabilities. Contract and design drafts are
stored by digest for subsequent review; oracle drafts must be registered and
approved before they can execute.

`promote` installs the reviewed execution setup into that preparation run without
resetting model usage. The baseline stage still requires the registered human
approvals. A configured `reviewer` makes an independent model review after clean
verification; findings and recommendations are advisory and cannot grant approval.

`inventory` discovers nested Cargo, npm, Python and CMake roots from immutable
source. It identifies Tauri's separate frontend/backend roots and leaves IPC,
bundler and browser requirements explicit. `profile resolve` validates operator
selections, build roots and immutable pins; extensions never select a target or
assert tool support. A profile's `build_root` selects its native Cargo/CMake or
runtime project while graph paths remain repository-relative.

`import <repository-id> <path> --dirty <untracked-paths.json>` captures tracked
working-tree bytes plus an explicit JSON array of untracked paths. It does not
change HEAD, the Git index or the checkout. Symlinks and changed-during-read inputs
are rejected. The captured input digest is part of baseline approval.

`plan <planning.json>` and pack `plan_checks` derive the complete matrix from a
registered capability catalog. Missing obligation/configuration coverage is an
unsupported-capability error. Registered checker identities, parameter schemas,
resource bounds, public consumers and protected case inventories remain mandatory.
A model cannot supply an arbitrary prewritten plan through this operation.

Every required final check also runs on the immutable baseline. Historical failures
need exact check/configuration/case expectations and a separate `baseline-plan`
approval tied to that base and plan. Infrastructure errors cannot become historical
exceptions. These records never excuse a failure on the final candidate.

Design validation checks that reuse citations name existing files and symbols in
the exact input snapshot. Structured `mechanism_decisions` record pressure, owner,
alternatives, cost, reuse and evidence; a direct function or module is a valid
choice. Citation checking is lexical. Semantic suitability still needs review.
Context selection ranks cited code, writable responsibility scope and task terms,
retaining exact source provenance and explicit omissions instead of taking the
first twelve files.

`stage-replan <setup.json>` registers a concrete replacement and prints its exact
approval requirements. `activate-replan <run> <setup-digest>` checks those approvals,
spends one authorized replan and restarts baseline verification atomically. It
preserves earlier inputs, results, events and spent budgets. It cannot expand the
budget, extend the deadline or change approval authority. Outstanding operations
must first be reconciled. Semantic questions from the implementer can produce an
architect draft; activation still requires the operator's exact replacement intake.

Architecture requests can bind an `architecture` ownership map to their protected
`NativeInput`. It enforces ARCH-001 through ARCH-005: private context/module
boundaries, transitive dependencies from pure domain components to adapters,
unapproved component cycles, forbidden dependencies of public contracts, and new
external dependency edges. Test-only edges are excluded unless explicitly selected.
Ownership must be complete and unambiguous at the collector's declared granularity.

C and C++ requests with `require_symbol_precision` also run the pinned libclang
collector under the configured translation-unit arguments. It records declaration,
reference, and signature-type relationships in addition to CMake target/include
edges. Unsupported syntax or collector limits block the check. Cargo graphs remain
package-level; JavaScript, TypeScript and Python graphs remain resolved project
imports. Those collectors cannot claim symbol-level precision. Import/build graphs
use conservative public-contract dependency checks; semantic graphs use collected
signature-type edges for ARCH-004.

The runner attaches each accepted graph to its authenticated receipt with the exact
candidate, configuration, checker and environment identities. The context builder
uses those graphs to include affected components and their transitive consumers.
Unmapped changes require full coverage, and final verification always retains the
complete declared matrix.

Native workspace operations are registered through `packs::workspace_command` and
protected `native_check` requests. Rust supports workspace formatting, checking,
Clippy, separate test-binary inventories, and per-package doctests with explicit
features, target and compiler flags. C/C++ support configured Clang-Tidy and full
CTest execution after building the selected targets. These project tests supplement
protected acceptance oracles and public consumers; they do not replace them.

Concurrency is supported when its approved risk route supplies distinct protected
cases for schedules, linearizability, cancellation and shutdown in every required
configuration, plus a candidate-bound `concurrency-review`. Missing properties or
configurations block acceptance. The conformance fixture demonstrates lost-update
detection and worker cancellation/joining. Passing those schedules does not prove
race freedom; tasks requiring sanitizer instrumentation or a stronger model must
register that additional evidence explicitly.

`gate` inspects existing evidence; `verify` performs fresh final verification for
an active candidate. Each explicit verification gets a new evidence nonce and
executes the entire required final matrix. Previous failures stay unresolved until
an authenticated disposition names their replacement evidence. Completed passes
from a prior attempt cannot satisfy the new attempt. Revoked plan approval,
unreconciled operations, exhausted budgets and a moved destination prevent it.
Terminal runs stay immutable. For CI rechecks, create a new run with an
`initial_candidate` and use `verify-run`, which never calls a model. Scope admission,
baseline checks, negative controls and all required final checks still apply.
If repair or an independent model review is needed, the offline invocation blocks
without reserving model usage; `run` can resume it with a provider.

`merge-prepare` reconstructs an exact integration tree in Git's object database
against an explicit `refs/heads/` destination and expected head. It retains unrelated
destination changes, rejects conflicts and leaves the checkout and index untouched.
Its proposed setup has a new run ID, base and initial candidate. It needs new
approvals and full verification. Changed source cited by the design requires a new
design decision before preparation can continue.

After acceptance, `prepare-delivery` creates an unreferenced commit for a concrete
`delivery` approval. `deliver` conditionally advances a bare integration repository's
branch. The final gate and exact approval are rechecked while approval changes are
serialized; the Git reference transaction rejects a changed head. An idempotent
retry still checks current authority and approvals. A checkout can use the prepared
commit or exported bundle in its usual reviewed integration workflow. Acceptance
and delivery do not authorize deployment.

Baseline expectations can also bind `observed_signature` from `failure_signature`
for an existing compiler/static failure with no executed cases. The signature
includes raw diagnostic artifacts; a changed failure requires new review. This
never waives candidate failures or infrastructure errors.

`resolve-replan` records an explicit disposition when an activated, reviewed replan
has replaced a failed check identity. It requires the complete passing replacement
matrix and a current `replan-resolution` approval. Unchanged check identities use
`resolve-failure`. Both retain the original failed result, and revocation immediately
blocks acceptance again.

## Native conformance and live-provider studies

`pack conformance <suite.json>` executes pinned positive and negative fixtures
through the registered native matrix, without model calls. A suite references
stored setup/candidate digests and an exact expected status and failing-case set
for each check/configuration. Every selected configuration needs a passing fixture
and a known wrong implementation. Infrastructure errors and missing tests cannot
count as successful mutant rejection. The JSON report includes authenticated
results, and a mismatched suite exits nonzero. Shared protocol/schema corruption
fixtures remain part of the ordinary Rust test suite.

`eval validate <study.json>` validates a proposed study and prints its identity and
maximum planned model spend without calling a provider. `eval run <study.json>
<chat-completions-url> <rates.json>` runs the selected provider with the same API-key
handling as `run`. The model ID, context/output limits, prices and per-stage caps
are explicit. The planned total must fit `maximum_total_cost`; unknown usage or
an overrun stops further model calls and leaves the real reservation visible.
A reused study/run identity is rejected, preventing an accidental budget reset.
Benchmark runs terminate as `stopped` with their measured reports linked into
history; they cannot be resumed as accepted production runs.

The three arms are existing instructions plus existing CI, approved contract/design
and profile guidance plus the same CI, and the same guidance plus the complete
required native matrix. All arms retain host isolation, protected-asset separation
and the same source-write scope. They share proposal limits, model settings and
budgets. The repeated trials rotate arm order. Benchmark decisions cannot grant
production acceptance, merge approval or deployment authority. Cases requiring
independent human/model review or policy exceptions must first be separated into
an explicitly reviewed evaluation; this automated native comparison does not
silently count those decisions as satisfied.

A `Study` references operator-registered `TaskSetup` artifacts and independent
held-out setup artifacts in the same ledger. Both use the same immutable base,
profiles, dependency assets and image; their protected test assets must differ.
The operator reviews that the withheld cases are meaningful independent ground
truth. Merely giving two files different hashes does not establish that fact.
Register setup files with `put operation`, snapshots with `put snapshot`, and
public request/instruction text with `put context` after importing their referenced
assets. The exact serialized types are in `study.rs` and `conformance.rs`.

Each case declares its language, workload, risk group, whether it is a negative
example for adding abstractions, and ordered stages. Later stages carry the actual
previous accepted proposal. Unrelated dependency changes are retained; conflicting
changes block instead of silently restoring a reference implementation. Include
prior behavior in follow-on test suites. A failed predecessor skips its dependent
stages, which remain visible in the denominator. Rust, C, C++, JavaScript,
TypeScript and Python use the same study protocol and their registered native packs.

Held-out tests run only after generation/repair for that stage. Their contract,
cases and diagnostics are never supplied to the provider. Reports preserve native
result IDs, incomplete judgments, repairs, domain-decision requests, changed-file
counts for extension work, actual model spend and full stage latency. Summaries
authenticate complete trial reports and check decisions and billing against the
ledger. They separate language, workload,
risk and arm; binomial estimates have descriptive 95% Wilson intervals, grouping
extension stages by case/repetition. Ratios use decimal strings in canonical JSON.
These intervals describe the selected repeated dataset, not a production guarantee.
Human review duration, tool monetary cost and semantic design quality need separate
review measurements; they are not invented from model confidence or file counts.

The automated tests exercise all six language families with scripted responses to
check context separation, budgets and report integrity. Those fixtures are explicitly
protocol smoke tests, not live-model quality results or a representative held-out
study. Running and publishing such a study still requires a selected provider,
credentials, an explicit spending cap, and reviewed representative repository tasks.
The existing 108-observation native verifier bank remains separately reported.
