import { preparationToRetry, type PlanPreparation } from './planPreparation';
import type { AgentStatus } from './agentRuns';
import { harnessRequest, type HarnessRun } from './polyglotHarness';
import { invoke } from '@tauri-apps/api/core';
import { createSignal } from 'solid-js';

/** Evidence saved on an implementation request, including remote mirrors. */
export interface PlanImplementation {
  dialogId: string;
  messageId: string;
  runId: string | null;
  preparationId?: string;
  status: 'implementing' | 'implemented' | 'review-needed' | 'interrupted';
}

const [revision, setRevision] = createSignal(0);
/** Invalidates open plan views after implementation evidence changes. */
export const planImplementationRevision = revision;

/** A run or mirror changed: re-read persistent evidence for visible plans. */
export function refreshPlanImplementations(): void {
  setRevision((value) => value + 1);
}

/** The same project-relative key is used by the chat database on all hosts. */
export function planKey(path: string) {
  return '.rovik/plans/' + path.split(/[\\/]/).pop();
}

export type PlanImplementationLink = Pick<
  PlanImplementation,
  'dialogId' | 'runId' | 'preparationId'
> & {
  status: PlanImplementation['status'] | 'preparation-paused';
  verified?: boolean;
};

type ActivePlan = {
  projectId: string;
  path: string;
  content?: string;
  dialogId: string;
  status: PlanImplementationLink['status'];
};
const active = new Map<string, ActivePlan>();

/** Publish only transitions so a status poll does not invalidate every open plan. */
export function setActiveVerifiedPlan(dialogId: string, plan?: ActivePlan) {
  const previous = active.get(dialogId);
  if (
    previous === plan ||
    (previous &&
      plan &&
      previous.path === plan.path &&
      previous.projectId === plan.projectId &&
      previous.content === plan.content &&
      previous.status === plan.status)
  )
    return;
  if (plan) active.set(dialogId, plan);
  else active.delete(dialogId);
  refreshPlanImplementations();
}

/** Verification completes when a candidate can be reviewed. Applying it is a separate decision. */
export function verifiedImplementationStatus(run: HarnessRun): PlanImplementation['status'] {
  if (run.status === 'verified' || run.status === 'applied') return 'implemented';
  return run.status === 'running' ? 'implementing' : 'interrupted';
}

/** Match the plan revision, then resolve verified attempts against their owning host. */
export async function getPlanImplementation(
  projectId: string | null,
  path: string,
  content: string,
  root?: string
): Promise<PlanImplementationLink | null> {
  const key = planKey(path);
  const text = content.replace(/\r\n/g, '\n');
  const matches = [...active.values()]
    .reverse()
    .filter(
      (plan) =>
        plan.projectId === projectId &&
        planKey(plan.path) === key &&
        (plan.content === undefined || plan.content.replace(/\r\n/g, '\n') === text)
    );
  const plan = matches.find((entry) => entry.status === 'implementing') ?? matches[0];
  if (plan) return { dialogId: plan.dialogId, runId: null, verified: true, status: plan.status };
  const record = await invoke<PlanImplementation | null>('chat_plan_implementation', {
    projectId,
    path,
    content,
  });
  if (!record?.preparationId || !root) return record;
  const target = { root, dialogId: record.dialogId };
  const session = await harnessRequest<PlanPreparation>(target, {
    action: 'preparationStatus',
    preparationId: record.preparationId,
  });
  if (planKey(session.plan.path) !== key || session.plan.content.replace(/\r\n/g, '\n') !== text)
    return null;
  if (!session.runId) {
    // An unpromoted session can outlive its agent. The owning chat may also be running a later request.
    if (['preparing', 'blocked', 'ready'].includes(session.status)) {
      const { runs } = await invoke<AgentStatus>('desktop_event_status', {
        scope: { projectId, chats: [record.dialogId] },
      });
      if (runs.some((run) => run.dialogId === record.dialogId)) {
        const request = await preparationToRetry(record.dialogId);
        if (request?.preparationId === record.preparationId)
          return { ...record, verified: true, status: 'implementing' };
      }
    }
    return { ...record, verified: true };
  }
  const run = await harnessRequest<HarnessRun>(target, { action: 'status', runId: session.runId });
  return { ...record, verified: true, runId: run.runId, status: verifiedImplementationStatus(run) };
}
