import { invoke } from '@tauri-apps/api/core';
import { createSignal } from 'solid-js';

/** Evidence saved on an implementation request, including remote mirrors. */
export interface PlanImplementation {
  dialogId: string;
  messageId: string;
  runId: string | null;
  status: 'implementing' | 'implemented' | 'review-needed' | 'interrupted';
}

const [revision, setRevision] = createSignal(0);
/** Invalidates open plan views after implementation evidence changes. */
export const planImplementationRevision = revision;

/** A run or mirror changed: re-read persistent evidence for visible plans. */
export function refreshPlanImplementations(): void {
  setRevision((value) => value + 1);
}

/** Matches a saved plan's path and exact content within the current project. */
export function getPlanImplementation(
  projectId: string | null,
  path: string,
  content: string
): Promise<PlanImplementation | null> {
  return invoke('chat_plan_implementation', { projectId, path, content });
}
