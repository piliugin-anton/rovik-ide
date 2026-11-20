import type { ChatMessage, ModelProfile, ToolMeta } from '../types';
import { chatApi } from './chatApi';
import { isRequestMessage } from './requests';
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

export function preparationPrompt(planPath: string) {
  return `Prepare verification for the plan in \`${planPath}\`.`;
}

/** Keep the conversation readable while its private tool transcript stays in durable history. */
export function visiblePreparationMessages(
  messages: ChatMessage[],
  id: string | undefined,
  running: boolean
) {
  if (!id) return messages;
  let privateTurn = false;
  return messages.filter((message, index) => {
    if (isRequestMessage(message)) privateTurn = message.meta?.verifiedPreparation === id;
    if (
      !privateTurn ||
      message.error ||
      message.failed ||
      message.meta?.failure ||
      message.meta?.paused ||
      message.meta?.interrupted
    )
      return true;
    if (message.role === 'user')
      return message.content !== preparationPrompt(message.meta?.planPath ?? '');
    // A stopped agent can ask a plain-text question as its final reply.
    return (
      !running &&
      index === messages.length - 1 &&
      message.role === 'assistant' &&
      !message.toolCalls?.length
    );
  });
}

/** Resolve the current request from durable history, including requests outside the loaded page. */
export async function preparationToRetry(
  dialogId: string
): Promise<{ preparationId: string; planPath: string } | undefined> {
  let before: number | undefined;
  for (;;) {
    const page = await chatApi.getMessages(dialogId, before, 500);
    const request = [...page.messages].reverse().find(isRequestMessage);
    if (request) {
      const id = request.meta?.verifiedPreparation;
      if (id === undefined) return undefined;
      const path = request.meta?.planPath;
      if (typeof id !== 'string' || !id || typeof path !== 'string' || !path)
        throw new Error(
          'This preparation request is incomplete. Reopen the saved plan to prepare it again.'
        );
      return { preparationId: id, planPath: path };
    }
    if (!page.hasOlder || !page.messages.length) return undefined;
    const next = page.messages[0].seq;
    if (next === undefined || (before !== undefined && next >= before))
      throw new Error('Could not load the preparation request. Reload this chat and retry.');
    before = next;
  }
}

/** An explicit exit applies only to this preparation, across window/app restarts. */
export function closePreparationReply(dialogId: string, preparationId: string): void {
  localStorage.setItem(`rovik.preparation.closed.${dialogId}`, preparationId);
}

/** Whether this exact preparation was explicitly closed, including before a restart. */
export function preparationReplyClosed(dialogId: string, preparationId: string): boolean {
  return localStorage.getItem(`rovik.preparation.closed.${dialogId}`) === preparationId;
}

/** Explicitly resuming the same preparation reverses its previous UI exit. */
export function reopenPreparationReply(dialogId: string, preparationId: string): void {
  const key = `rovik.preparation.closed.${dialogId}`;
  if (localStorage.getItem(key) === preparationId) localStorage.removeItem(key);
}

/** Recover a private continuation from durable history, never an older unrelated request. */
export async function restorePreparationReply(
  target: HarnessTarget & { dialogId: string },
  meta: ToolMeta | undefined,
  current: () => boolean,
  load = preparationToRetry,
  status = (id: string) =>
    harnessRequest<PlanPreparation>(target, {
      action: 'preparationStatus',
      preparationId: id,
    })
): Promise<PlanPreparation | undefined> {
  if (meta?.command || meta?.verifiedPreparation !== undefined) return;
  const previous = await load(target.dialogId);
  if (!current())
    throw new Error(
      'The chat or project changed while restoring preparation. Send again in the intended chat.'
    );
  if (!previous || preparationReplyClosed(target.dialogId, previous.preparationId)) return;
  const preparation = await status(previous.preparationId);
  if (!current())
    throw new Error(
      'The chat or project changed while restoring preparation. Send again in the intended chat.'
    );
  if (
    preparation.preparationId !== previous.preparationId ||
    preparation.plan.path !== previous.planPath
  )
    throw new Error('The saved preparation binding changed. Reopen the plan before continuing.');
  if (!preparation.runId && ['preparing', 'blocked'].includes(preparation.status))
    return preparation;
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
