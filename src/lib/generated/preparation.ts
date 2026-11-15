// Generated from Rust preparation contracts. Run npm run generate:preparation-types to update.

// prettier-ignore
export type Language = "rust" | "cpp" | "c" | "javascript" | "typescript" | "python";

// prettier-ignore
export type Configuration = {
/**
 * Human-readable configuration ID.
 */
id: string,
/**
 * Native target identity, never an implicit host default.
 */
target: string,
/**
 * Explicit features or preprocessor definitions.
 */
features: Array<string>,
/**
 * Whether default features are enabled.
 */
default_features: boolean,
/**
 * Registered build target IDs.
 */
build_targets: Array<string>,
/**
 * Approved compiler flags, including instrumentation where required.
 */
flags: Array<string>,
/**
 * Registered native/device/emulator runtime.
 */
runtime: string, };

// prettier-ignore
export type WorkerLimits = {
/**
 * Registered model identity.
 */
model_id: string,
/**
 * Maximum complete serialized context, including tool replies.
 */
context_bytes: number,
/**
 * Maximum provider output tokens.
 */
output_tokens: number,
/**
 * Maximum duration of one model attempt.
 */
timeout_ms: number,
/**
 * Reservation for total input plus output tokens.
 */
token_reservation: number,
/**
 * Maximum expected cost for one model attempt, in configured microunits.
 */
cost_reservation: number,
/**
 * At most one schema-format repair per request sequence.
 */
format_repairs: number,
/**
 * Bounded retries of completed infrastructure failures for one exact check binding.
 */
infrastructure_retries_per_check: number, };

// prettier-ignore
export type WorkspaceBuildExclusion = {
/**
 * Exact manifest observed in the captured project.
 */
manifest: string,
/**
 * Why this immutable root is unrelated to the requested implementation.
 */
rationale: string, };

// prettier-ignore
export type WorkspaceProfileInput = {
/**
 * Stable profile identity, used in requirement mapping keys.
 */
id: string,
/**
 * Repository-relative discovered manifest path.
 */
manifest: string,
/**
 * Registered adapter family.
 */
language: Language,
/**
 * Explicit language facts; required names are supplied by the language profile.
 */
facts: { [key in string]?: string },
/**
 * Exact supported targets and features.
 */
configurations: Array<Configuration>,
/**
 * Existing committed test files. Native tests stay frozen in the source tree;
 * runtime .mjs/.py tests are copied to the protected oracle and must exercise
 * `/build/project` (TypeScript output is `/build/project/dist`).
 */
protectedTestPaths: Array<string>,
/**
 * Existing protected TypeScript compiler configuration, when applicable.
 */
typescriptProject?: string | null, };

// prettier-ignore
export type WorkspaceRequirement = {
/**
 * Stable requirement ID.
 */
id: string,
/**
 * Behavior to establish, not an inferred domain assumption.
 */
statement: string,
/**
 * One declared component responsible for this behavior.
 */
owner: string,
/**
 * Nonempty exact native case IDs for every selected configuration.
 */
casesByConfiguration: { [key in string]?: Array<string> }, };

// prettier-ignore
export type WorkspaceCitation = {
/**
 * Existing repository-relative file.
 */
file: string,
/**
 * Existing symbol whose lexical presence is independently checked.
 */
symbol: string,
/**
 * Why this code can or cannot satisfy the requirement.
 */
rationale: string, };

// prettier-ignore
export type WorkspaceDesignInput = {
/**
 * Component responsibility descriptions.
 */
components: { [key in string]?: string },
/**
 * Explicit source reuse analysis.
 */
existingCode: Array<WorkspaceCitation>,
/**
 * Public API and compatibility decisions.
 */
publicApiChanges: Array<string>,
/**
 * Ownership and resource lifecycle decisions.
 */
resourceContracts: Array<string>,
/**
 * Failure behavior decisions.
 */
errorContracts: Array<string>,
/**
 * Synchronization and cancellation assumptions.
 */
concurrencyContract: string,
/**
 * Reason a direct function/module is sufficient.
 */
mechanismPressure: string,
/**
 * Simpler alternatives considered and their tradeoffs.
 */
mechanismAlternatives: Array<string>,
/**
 * Ownership and maintenance cost of this decision.
 */
mechanismCost: string, };

// prettier-ignore
export type WorkspaceEnvironmentInput = {
/**
 * Locally prepared image's immutable sha256 config digest.
 */
imageDigest: string,
/**
 * Explicit execution engine, either docker or podman.
 */
engine: string,
/**
 * Absolute selected host file describing the pinned toolchain.
 */
toolchainManifestPath: string,
/**
 * Absolute selected prepared offline dependency directory.
 */
dependencyDirectory: string, };

// prettier-ignore
export type WorkspaceBaselineFailure = {
/**
 * Selected profile identity.
 */
profileId: string,
/**
 * Selected configuration identity.
 */
configurationId: string,
/**
 * Exact failures expected on the unchanged baseline.
 */
caseIds: Array<string>,
/**
 * Why these failures are part of the requested change.
 */
rationale: string, };

// prettier-ignore
export type WorkspaceSetupInput = {
/**
 * Stable task label.
 */
name: string,
/**
 * Requested outcome from the reviewed implementation plan.
 */
intent: string,
/**
 * Explicit domain context; manifests cannot establish domain meaning.
 */
boundedContext: string,
/**
 * Literal file paths or directory patterns ending in `/**`.
 */
writePaths: Array<string>,
/**
 * Additional paths which implementation must preserve.
 */
protectedPaths: Array<string>,
/**
 * Every build root needs a resolved selection or an explicit scoped exclusion.
 */
profiles: Array<WorkspaceProfileInput>,
/**
 * Explicitly unverified build roots, frozen outside the implementation scope.
 */
excludedBuildRoots?: Array<WorkspaceBuildExclusion>,
/**
 * Required behavior and cases for each configuration.
 */
requirements: Array<WorkspaceRequirement>,
/**
 * Reviewed ownership and reuse decisions.
 */
design: WorkspaceDesignInput,
/**
 * Prepared host assets selected by the operator.
 */
environment: WorkspaceEnvironmentInput,
/**
 * An existing Git revision whose relevant behavior is known to fail.
 */
negativeControlRevision: string,
/**
 * Cases from one behavioral check which must actually fail on the control.
 */
negativeControlCases: Array<string>,
/**
 * Explicit historical failures; all other baseline checks must pass.
 */
baselineFailures?: Array<WorkspaceBaselineFailure>,
/**
 * Selected model and bounded provider requests.
 */
worker: WorkerLimits,
/**
 * Explicit model_calls, tokens, cost_microunits and checks accounts.
 */
budgets: Record<string, number>,
/**
 * Maximum run lifetime; registration never creates an unlimited deadline.
 */
durationMs: number, };

// prettier-ignore
export type PreparationStagedFile = { path: string, source: string, };

// prettier-ignore
export type PreparationRecipe = { setup: WorkspaceSetupInput, oracleFiles?: Array<PreparationStagedFile>,
/**
 * Real source edits exercised by the controller's negative-control check.
 */
negativeControlFiles: Array<PreparationStagedFile>, };

// prettier-ignore
export const PROFILE_FACTS = {
  "rust": [
    "edition",
    "minimum_supported_rust_version",
    "execution_target",
    "std_or_no_std",
    "allocation_policy",
    "panic_policy",
    "unsafe_scope"
  ],
  "cpp": [
    "standard_library_lock",
    "language_standard",
    "target_abi",
    "exceptions_policy",
    "rtti_policy",
    "allocation_policy",
    "abi_compatibility_policy",
    "threading_model"
  ],
  "c": [
    "language_standard",
    "target_abi",
    "hosted_or_freestanding",
    "allocation_policy",
    "threading_and_interrupt_policy"
  ],
  "javascript": [
    "node_version",
    "npm_version",
    "module_system",
    "test_runner",
    "dependency_policy"
  ],
  "typescript": [
    "node_version",
    "npm_version",
    "typescript_version",
    "module_system",
    "test_runner",
    "dependency_policy"
  ],
  "python": [
    "python_version",
    "pytest_version",
    "test_runner",
    "dependency_policy",
    "import_policy"
  ]
} as const satisfies Record<Language, readonly string[]>;
