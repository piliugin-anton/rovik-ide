import { invoke } from '@tauri-apps/api/core';
import type { DialogSummary } from '../types';

/** A known chat on the currently selected project's execution host. */
export interface HarnessChatContext {
  dialogId: string;
  projectId: string;
  hostId: string;
  label: string;
}

/** Project switches and chats in transit cannot nominate an execution context. */
export function eligibleHarnessChat(
  projectId: string | null | undefined,
  chatProjectId: string | null,
  chat: DialogSummary | null | undefined,
  hosts: readonly { id: string; name: string }[]
): HarnessChatContext | undefined {
  if (!projectId || projectId !== chatProjectId || chat?.place?.kind !== 'host') return;
  const host = hosts.find((entry) => entry.id === chat.place!.host);
  if (!host) return;
  return { dialogId: chat.id, projectId, hostId: host.id, label: `${host.name} — ${chat.title}` };
}

/** The trusted controller's project API; this is never exposed as an agent tool. */
export interface HarnessTarget {
  root: string;
  /** A chat routes the request to its owning execution host. */
  dialogId?: string | null;
}

export interface HarnessEngine {
  kind: 'docker' | 'podman';
  executable: string;
}

export interface HarnessApprovalRequest {
  run_id: string | null;
  subject_digest: string;
  approval_type: string;
  policy_digest: string;
  scope_digest: string;
  expires_ms: number | null;
}

export interface HarnessApproval {
  approvalType: string;
  subjectDigest: string;
  scopeDigest: string;
  policyDigest: string;
  summary: string;
  subjectKind: string;
  subject: unknown;
  assets: { path: string; digest: string; content?: string | null; omitted?: string | null }[];
  request: HarnessApprovalRequest;
}

export type HarnessRunStatus =
  'running' | 'verified' | 'failed' | 'blocked' | 'stale' | 'cancelled';

export interface HarnessRun {
  runId: string;
  state: string;
  status: HarnessRunStatus;
  snapshotDigest: string;
  candidateDigest?: string | null;
  reason?: string | null;
  reasons?: string[];
  checks?: {
    check_id: string;
    status: string;
    executed_test_count: number;
    failed_case_ids: string[];
    diagnostics: { rule_id: string; message: string; file?: string; line?: number }[];
  }[];
  pendingApprovals: HarnessApproval[];
  canResume: boolean;
  canRecover: boolean;
  canCancel: boolean;
}

export interface HarnessSetup {
  id: string;
  label: string;
  contractSummary: string;
  checks: {
    id: string;
    command: string;
    phase: string;
    argv?: string[];
    caseIds?: string[];
    minimumTestCount?: number;
  }[];
  configurations: string[];
  baseRevision?: string;
  scope?: { allow_write: string[]; protected: string[] };
  protectedPaths?: string[];
  invariants?: { id: string; statement: string; owner: string; case_ids: string[] }[];
  candidateReviews?: string[];
  negativeControlCases?: string[];
}

export interface HarnessInspection {
  projectId: string;
  snapshotDigest: string;
  inventory: {
    units: { root: string; manifest: string; languages: string[]; locks: string[] }[];
    unresolved: string[];
  };
  untracked: string[];
  host: {
    supported: boolean;
    platform: string;
    engines: HarnessEngine[];
    reason?: string | null;
  };
  configuration: {
    digest: string;
    setupId: string;
    label: string;
    engine: HarnessEngine;
  } | null;
  setups: HarnessSetup[];
  runs: HarnessRun[];
  issues: string[];
}

export type HarnessOperation =
  | { action: 'inspect'; untracked: string[] }
  | { action: 'importSetup'; path: string }
  | {
      action: 'configure';
      setupId: string;
      engine: HarnessEngine;
      expectedSnapshotDigest: string;
    }
  | {
      action: 'verify';
      untracked: string[];
      expectedSnapshotDigest: string;
      savedBuffers: true;
    }
  | { action: 'status' | 'approvals' | 'cancel' | 'resume' | 'recover'; runId: string }
  | { action: 'approve'; runId: string; request: HarnessApprovalRequest };

/** The same RPC reaches the local backend or the selected chat's cloud host. */
export function harnessRequest<T>(target: HarnessTarget, request: HarnessOperation): Promise<T> {
  return invoke<T>('polyglot_harness', {
    root: target.root,
    executionHost: target.dialogId ? 'chat' : 'local',
    dialogId: target.dialogId ?? null,
    request,
  });
}

/** Unsaved text is not part of a filesystem snapshot and cannot be silently omitted. */
export function verificationInputProblem(
  unsavedFiles: readonly string[] | undefined
): string | null {
  if (unsavedFiles === undefined)
    return 'Editor state is unavailable. Open verification from the project’s settings.';
  if (unsavedFiles.length > 0) {
    return `Save or undo the unsaved changes in ${unsavedFiles.length} ${unsavedFiles.length === 1 ? 'file' : 'files'} before verifying.`;
  }
  return null;
}

/** A controller verdict belongs to its snapshot, never to later editor changes. */
export function displayedHarnessStatus(
  run: HarnessRun,
  unsavedFiles: readonly string[] | undefined,
  snapshotDigest?: string
): HarnessRunStatus {
  if (
    run.status === 'verified' &&
    (verificationInputProblem(unsavedFiles) !== null ||
      (snapshotDigest !== undefined && snapshotDigest !== run.snapshotDigest))
  )
    return 'stale';
  return run.status;
}

/** Starting a fresh verification always binds the exact inspected input. */
export function verifyChanges(
  target: HarnessTarget,
  inspection: HarnessInspection,
  untracked: readonly string[],
  unsavedFiles: readonly string[] | undefined
): Promise<HarnessRun> {
  const problem = verificationInputProblem(unsavedFiles);
  if (problem) return Promise.reject(new Error(problem));
  if (!inspection.host.supported || !inspection.configuration) {
    return Promise.reject(
      new Error('Select a supported execution host and review the project setup first.')
    );
  }
  return harnessRequest(target, {
    action: 'verify',
    untracked: [...untracked],
    expectedSnapshotDigest: inspection.snapshotDigest,
    savedBuffers: true,
  });
}
