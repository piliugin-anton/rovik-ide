import type { ModelProfile } from '../types';
import {
  harnessRequest,
  verificationInputProblem,
  type HarnessInspection,
  type HarnessRun,
  type HarnessTarget,
  type HarnessProviderBinding,
} from './polyglotHarness';
import type { WorkspaceSetupInput } from './polyglotPreparation';

/** Exact saved bytes read by the execution host. */
export interface HarnessPlan {
  path: string;
  content: string;
  digest: string;
}
export interface HarnessRates {
  input: number;
  cachedInput: number;
  output: number;
}
/** Credentials remain in memory and use the existing authenticated RPC secret flow. */
export interface HarnessProviderInput {
  profile: ModelProfile;
  rates: HarnessRates;
}
export interface HarnessChangeReview {
  runId: string;
  candidateDigest?: string;
  patch?: string;
  files?: {
    path: string;
    before?: string | null;
    after?: string | null;
    omitted?: string | null;
  }[];
  [key: string]: unknown;
}
export type HarnessPlanOperation =
  | { action: 'inspectPlan'; planPath: string }
  | { action: 'prepareWorkspace'; input: WorkspaceSetupInput }
  | {
      action: 'implementPlan';
      planPath: string;
      expectedPlanDigest: string;
      expectedSnapshotDigest: string;
      untracked: string[];
      savedBuffers: true;
      planCriteriaReviewed: true;
      provider: HarnessProviderInput;
    }
  | { action: 'resumeImplementation'; runId: string; provider: HarnessProviderInput }
  | { action: 'reviewChanges' | 'exportReview'; runId: string }
  | { action: 'prepareApplication'; runId: string }
  | { action: 'releaseApplication'; runId: string; subjectDigest: string; acknowledged: true }
  | {
      action: 'applyApplication' | 'recoverApplication';
      runId: string;
      subjectDigest: string;
      savedBuffers: true;
    }
  | {
      action: 'draftPlan';
      runId: string;
      role: 'domain' | 'architect' | 'oracle';
      provider: HarnessProviderInput;
    };

/** The strict worker currently speaks the OpenAI-compatible Chat Completions API. */
export function harnessProviderProblem(profile: ModelProfile): string | null {
  if (profile.connector || profile.baseUrl.startsWith('connector:'))
    return 'Choose an OpenAI-compatible model provider. Connector agents cannot run the verified worker.';
  if (profile.apiRoute === 'responses' || /api\.anthropic\.com/i.test(profile.baseUrl))
    return 'Choose a provider using Chat Completions. This verified worker does not support the selected API route.';
  try {
    const endpoint = new URL(profile.baseUrl);
    if (endpoint.username || endpoint.password || endpoint.search || endpoint.hash)
      return 'Provider credentials must use the configured API key field; remove credentials, query parameters and fragments from the endpoint.';
  } catch {
    return 'The configured provider endpoint is not a valid URL.';
  }
  return null;
}

/** Rotated credentials are allowed; changing the endpoint, model or profile is not. */
export function boundHarnessProviderProblem(
  binding: HarnessProviderBinding,
  profile: ModelProfile | undefined
): string | null {
  if (!profile)
    return 'The bound provider is unavailable. Restore its configured model and credentials to resume.';
  const problem = harnessProviderProblem(profile);
  if (problem) return problem;
  if (
    binding.profileId !== profile.id ||
    binding.model !== profile.model ||
    binding.endpoint !== `${profile.baseUrl.replace(/\/+$/, '')}/chat/completions`
  )
    return 'This run is bound to a different provider, model or endpoint. Restore its original configured provider to resume.';
  return null;
}

/** Explicit zero rates support local models; missing rates never imply zero. */
export function parseHarnessRates(
  input: string,
  cachedInput: string,
  output: string
): HarnessRates | null {
  const values = [input, cachedInput, output];
  if (values.some((value) => !/^\d+$/.test(value.trim()) || !Number.isSafeInteger(Number(value))))
    return null;
  return { input: Number(input), cachedInput: Number(cachedInput), output: Number(output) };
}

/** Bind plan, input and criteria before making any model request. */
export function implementVerifiedPlan(args: {
  target: HarnessTarget;
  plan: HarnessPlan;
  inspection: HarnessInspection;
  untracked: readonly string[];
  unsavedFiles: readonly string[] | undefined;
  provider: HarnessProviderInput;
  reviewed: boolean;
}): Promise<HarnessRun> {
  const problem =
    verificationInputProblem(args.unsavedFiles) ?? harnessProviderProblem(args.provider.profile);
  if (problem) return Promise.reject(new Error(problem));
  if (!args.reviewed)
    return Promise.reject(
      new Error('Review the exact saved plan and confirm the acceptance criteria cover it.')
    );
  if (!args.inspection.configuration || !args.inspection.host.supported)
    return Promise.reject(new Error('Review and configure a supported verification setup first.'));
  return harnessRequest(args.target, {
    action: 'implementPlan',
    planPath: args.plan.path,
    expectedPlanDigest: args.plan.digest,
    expectedSnapshotDigest: args.inspection.snapshotDigest,
    untracked: [...args.untracked],
    savedBuffers: true,
    planCriteriaReviewed: true,
    provider: args.provider,
  });
}

/** A strict session is pinned to the reviewed project and chat owner across UI navigation. */
export interface VerifiedPlanContext {
  root: string;
  projectId: string;
  owner: string;
}
export function verifiedPlanContextProblem(
  captured: VerifiedPlanContext,
  current: {
    root: string | null;
    projectId: string | null | undefined;
    chatProjectId: string | null;
    owner: string | undefined;
  }
): string | null {
  if (
    captured.root !== current.root ||
    captured.projectId !== current.projectId ||
    current.chatProjectId !== current.projectId
  )
    return 'The selected project changed. Return to the original project to review or manage this persisted implementation.';
  if (captured.owner !== current.owner)
    return 'The chat execution host changed or is unavailable. Reopen verified implementation on its current host; the previous run remains persisted.';
  return null;
}

/** Recheck provenance and editor state after asynchronous preparation, immediately before dispatch. */
export async function dispatchReviewedTask<Prepared, Result>(args: {
  captured: VerifiedPlanContext & { dialogId: string };
  current: () => Parameters<typeof verifiedPlanContextProblem>[1] & { dialogId: string | null };
  unsavedFiles: () => readonly string[] | undefined;
  prepare: () => Promise<Prepared>;
  dispatch: (prepared: Prepared) => Promise<Result>;
}): Promise<Result> {
  const check = () => {
    const current = args.current();
    const problem =
      current.dialogId !== args.captured.dialogId
        ? 'The active chat changed while preparing the reviewed task.'
        : (verifiedPlanContextProblem(args.captured, current) ??
          verificationInputProblem(args.unsavedFiles()));
    if (problem) throw new Error(problem);
  };
  check();
  const prepared = await args.prepare();
  check();
  return args.dispatch(prepared);
}
