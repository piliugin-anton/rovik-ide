/** Values are suggested only; the execution host validates all pins and protected inputs. */
export type HarnessLanguage = 'rust' | 'c' | 'cpp' | 'javascript' | 'typescript' | 'python';
export interface HarnessConfiguration {
  id: string;
  target: string;
  features: string[];
  default_features: boolean;
  build_targets: string[];
  flags: string[];
  runtime: string;
}
export interface WorkspaceProfileInput {
  id: string;
  manifest: string;
  language: HarnessLanguage;
  facts: Record<string, string>;
  configurations: HarnessConfiguration[];
  protectedTestPaths: string[];
  typescriptProject?: string;
}
export interface WorkspaceSetupInput {
  name: string;
  intent: string;
  boundedContext: string;
  writePaths: string[];
  protectedPaths: string[];
  profiles: WorkspaceProfileInput[];
  requirements: {
    id: string;
    statement: string;
    owner: string;
    casesByConfiguration: Record<string, string[]>;
  }[];
  design: {
    components: Record<string, string>;
    existingCode: { file: string; symbol: string; rationale: string }[];
    publicApiChanges: string[];
    resourceContracts: string[];
    errorContracts: string[];
    concurrencyContract: string;
    mechanismPressure: string;
    mechanismAlternatives: string[];
    mechanismCost: string;
  };
  environment: {
    imageDigest: string;
    engine: 'docker' | 'podman';
    toolchainManifestPath: string;
    dependencyDirectory: string;
  };
  negativeControlRevision: string;
  negativeControlCases: string[];
  baselineFailures: {
    profileId: string;
    configurationId: string;
    caseIds: string[];
    rationale: string;
  }[];
  worker: {
    model_id: string;
    context_bytes: number;
    output_tokens: number;
    timeout_ms: number;
    token_reservation: number;
    cost_reservation: number;
    format_repairs: number;
    infrastructure_retries_per_check: number;
  };
  budgets: Record<string, number>;
  durationMs: number;
}
export const PROFILE_FACTS: Record<HarnessLanguage, readonly string[]> = {
  rust: [
    'edition',
    'minimum_supported_rust_version',
    'execution_target',
    'std_or_no_std',
    'allocation_policy',
    'panic_policy',
    'unsafe_scope',
  ],
  c: [
    'language_standard',
    'target_abi',
    'hosted_or_freestanding',
    'allocation_policy',
    'threading_and_interrupt_policy',
  ],
  cpp: [
    'standard_library_lock',
    'language_standard',
    'target_abi',
    'exceptions_policy',
    'rtti_policy',
    'allocation_policy',
    'abi_compatibility_policy',
    'threading_model',
  ],
  javascript: ['node_version', 'npm_version', 'module_system', 'test_runner', 'dependency_policy'],
  typescript: [
    'node_version',
    'npm_version',
    'typescript_version',
    'module_system',
    'test_runner',
    'dependency_policy',
  ],
  python: ['python_version', 'pytest_version', 'test_runner', 'dependency_policy', 'import_policy'],
};
export function lines(text: string): string[] {
  return text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
}
export function suggestedProfile(
  language: HarnessLanguage,
  id = 'main',
  manifest = ''
): WorkspaceProfileInput {
  const runtime = ['javascript', 'typescript', 'python'].includes(language);
  const facts: Record<string, string> = {};
  if (language === 'javascript' || language === 'typescript')
    Object.assign(facts, {
      module_system: 'esm',
      test_runner: 'node:test',
      dependency_policy: 'npm-ci-offline-no-scripts',
    });
  if (language === 'python')
    Object.assign(facts, {
      test_runner: 'pytest',
      dependency_policy: 'hashed-wheels-offline',
      import_policy: 'isolated-pytest',
    });
  return {
    id,
    manifest,
    language,
    facts,
    protectedTestPaths: [],
    configurations: [
      {
        id: `${id}-tests`,
        target: runtime
          ? 'linux-x86_64'
          : language === 'rust'
            ? 'x86_64-unknown-linux-gnu'
            : 'x86_64-linux-gnu',
        features: [],
        default_features: language === 'rust',
        build_targets: [runtime ? 'protected-tests' : 'workspace-tests'],
        flags: language === 'typescript' ? ['--strict', '--noEmitOnError'] : [],
        runtime: runtime ? (language === 'python' ? 'cpython' : 'node') : 'linux-native',
      },
    ],
  };
}
export function suggestedWorkspaceSetup(model: string): WorkspaceSetupInput {
  return {
    name: '',
    intent: '',
    boundedContext: '',
    writePaths: [],
    protectedPaths: [],
    profiles: [suggestedProfile('typescript')],
    requirements: [{ id: 'requirement-1', statement: '', owner: '', casesByConfiguration: {} }],
    design: {
      components: {},
      existingCode: [],
      publicApiChanges: [],
      resourceContracts: [],
      errorContracts: [],
      concurrencyContract: '',
      mechanismPressure: '',
      mechanismAlternatives: [],
      mechanismCost: '',
    },
    environment: {
      imageDigest: '',
      engine: 'docker',
      toolchainManifestPath: '',
      dependencyDirectory: '',
    },
    negativeControlRevision: '',
    negativeControlCases: [],
    baselineFailures: [],
    worker: {
      model_id: model,
      context_bytes: 131072,
      output_tokens: 8192,
      timeout_ms: 180000,
      token_reservation: 16384,
      cost_reservation: 100000,
      format_repairs: 1,
      infrastructure_retries_per_check: 1,
    },
    budgets: { model_calls: 16, tokens: 262144, cost_microunits: 1000000, checks: 128 },
    durationMs: 3600000,
  };
}
/** Catch unresolved form decisions before calling the host's authoritative validator. */
export function workspaceSetupProblem(input: WorkspaceSetupInput): string | null {
  if (!input.name.trim() || !input.intent.trim() || !input.boundedContext.trim())
    return 'Name the setup, its intent and the bounded context.';
  if (!input.writePaths.length || !input.protectedPaths.length)
    return 'Select explicit writable and protected paths.';
  if (!input.profiles.length) return 'Add at least one language profile.';
  const keys = input.profiles.flatMap((profile) =>
    profile.configurations.map((configuration) => `${profile.id}/${configuration.id}`)
  );
  if (new Set(keys).size !== keys.length)
    return 'Profile and configuration identifiers must be unique.';
  for (const profile of input.profiles) {
    if (
      !profile.id ||
      !profile.manifest ||
      !profile.protectedTestPaths.length ||
      !profile.configurations.length
    )
      return `Complete ${profile.id || 'the profile'} with a manifest, protected tests and configurations.`;
    if (PROFILE_FACTS[profile.language].some((key) => !profile.facts[key]?.trim()))
      return `Resolve every ${profile.language} profile fact.`;
    if (
      profile.configurations.some(
        (configuration) =>
          !configuration.id ||
          !configuration.target ||
          !configuration.runtime ||
          !configuration.build_targets.length
      )
    )
      return 'Resolve every configuration target, runtime and build target.';
  }
  if (
    !input.requirements.length ||
    input.requirements.some(
      (requirement) =>
        !requirement.id ||
        !requirement.statement ||
        !requirement.owner ||
        keys.some((key) => !requirement.casesByConfiguration[key]?.length)
    )
  )
    return 'Map each requirement to concrete case IDs in every profile configuration.';
  if (
    !input.environment.imageDigest ||
    !input.environment.toolchainManifestPath ||
    !input.environment.dependencyDirectory
  )
    return 'Pin the container image, toolchain manifest and offline dependency directory on the execution host.';
  if (!input.negativeControlRevision || !input.negativeControlCases.length)
    return 'Select a known failing Git revision and the cases proving the tests detect it.';
  if (
    Object.keys(input.design.components).length === 0 ||
    !input.design.existingCode.length ||
    !input.design.concurrencyContract ||
    !input.design.mechanismPressure ||
    !input.design.mechanismAlternatives.length ||
    !input.design.mechanismCost
  )
    return 'Review the design, existing code evidence and mechanism tradeoffs.';
  if (
    [
      ...Object.values(input.worker).filter((value) => typeof value === 'number'),
      ...Object.values(input.budgets),
      input.durationMs,
    ].some((value) => !Number.isSafeInteger(value) || Number(value) < 0)
  )
    return 'Limits must be nonnegative whole numbers.';
  return null;
}
