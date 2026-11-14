import type { ModelProfile, ToolMeta } from '../types';
import { savedHarnessRates } from './harnessRates';
import {
  harnessRequest,
  verificationInputProblem,
  type HarnessTarget,
  type HarnessRun,
  type HarnessProviderBinding,
} from './polyglotHarness';
import {
  boundHarnessProviderProblem,
  harnessProviderProblem,
  verifiedPlanContextProblem,
  type HarnessPlan,
  type VerifiedPlanContext,
} from './polyglotPlan';

/** A host-created preparation session; private workspace paths never come from the UI. */
export interface PlanPreparation {
  preparationId: string;
  status: 'preparing' | 'ready' | 'blocked' | 'cancelled' | 'stale';
  plan: HarnessPlan;
  snapshotDigest: string;
  runId?: string;
  reason?: string;
  error?: string | null;
  providerBinding?: HarnessProviderBinding;
  review?: {
    digest: string;
    label: string;
    summary: string;
    setupId: string;
    scope: { writable: string[]; protected: string[] };
    checks: { id: string; label: string; configuration?: string }[];
    requirements: string[];
    warnings: string[];
    budgets?: Record<string, number>;
  };
}

/** Idle replies remain in the open preparation's private workspace until it finishes or closes. */
export function preparationReplyMetadata(
  preparation: PlanPreparation | undefined,
  plan: (VerifiedPlanContext & { dialogId: string; path?: string }) | undefined,
  current: Parameters<typeof verifiedPlanContextProblem>[1] & { dialogId: string | null },
  meta: ToolMeta | undefined
): ToolMeta | undefined {
  if (
    !preparation ||
    !plan?.path ||
    !['preparing', 'blocked'].includes(preparation.status) ||
    meta?.command ||
    meta?.verifiedPreparation !== undefined ||
    plan.dialogId !== current.dialogId ||
    verifiedPlanContextProblem(plan, current)
  )
    return meta;
  return {
    ...meta,
    mode: 'agent',
    verifiedPreparation: preparation.preparationId,
    planPath: plan.path,
  };
}

/** A saved preparation freezes prices as well as the provider identity. */
export function preparationProviderProblem(
  preparation: PlanPreparation | undefined,
  profile: ModelProfile | undefined
): string | null {
  const binding = preparation?.providerBinding;
  if (!binding) return null;
  if (boundHarnessProviderProblem(binding, profile))
    return 'The preparation model changed. Restore it or prepare this plan again.';
  const rates = savedHarnessRates(profile!);
  if (
    !rates ||
    ['input', 'cachedInput', 'output'].some(
      (key) => rates[key as keyof typeof rates] !== binding.rates[key as keyof typeof rates]
    )
  )
    return 'The saved model prices changed. Prepare this plan again to review the new limits.';
  return null;
}

export type PlanPreparationOperation =
  | {
      action: 'beginPlanPreparation';
      planPath: string;
      dialogId: string;
      provider: { profile: ModelProfile; rates: NonNullable<ModelProfile['harnessRates']> };
      savedBuffers: true;
      untracked: string[];
    }
  | { action: 'preparationStatus' | 'cancelPreparation'; preparationId: string }
  | {
      action: 'startPreparedImplementation';
      preparationId: string;
      expectedReviewDigest: string;
      provider: { profile: ModelProfile; rates: NonNullable<ModelProfile['harnessRates']> };
      savedBuffers: true;
    };

/** Decisions that need attention before the agent may prepare a paid implementation. */
export function preparationProblem(
  profile: ModelProfile | undefined,
  unsaved: readonly string[] | undefined
): string | null {
  return (
    verificationInputProblem(unsaved) ??
    (!profile
      ? 'Choose a model to prepare this plan.'
      : (harnessProviderProblem(profile) ??
        (!savedHarnessRates(profile)
          ? 'Save verification prices for this model in Model providers. Enter zero explicitly for a free model.'
          : null)))
  );
}

/** The user's review is bound to the exact prepared inputs; the host rechecks them before execution. */
export function startPreparedImplementation(
  target: HarnessTarget,
  preparation: PlanPreparation,
  profile: ModelProfile,
  unsaved: readonly string[] | undefined
): Promise<HarnessRun> {
  const problem =
    preparationProblem(profile, unsaved) ?? preparationProviderProblem(preparation, profile);
  if (problem) return Promise.reject(new Error(problem));
  if (preparation.status !== 'ready' || !preparation.review?.digest)
    return Promise.reject(
      new Error('Preparation must finish before this plan can be implemented.')
    );
  return harnessRequest(target, {
    action: 'startPreparedImplementation',
    preparationId: preparation.preparationId,
    expectedReviewDigest: preparation.review.digest,
    provider: { profile, rates: savedHarnessRates(profile)! },
    savedBuffers: true,
  });
}
