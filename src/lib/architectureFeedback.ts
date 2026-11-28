import { createSignal } from 'solid-js';
import { harnessRequest, type HarnessTarget } from './polyglotHarness';

/** Authenticated architecture evidence; this view cannot create an acceptance verdict. */
export interface ArchitectureFeedback {
  runId: string;
  projectId: string;
  workspace: string;
  currentSnapshotDigest: string | null;
  evidenceCandidateDigest: string | null;
  /** Supplied only after the host authenticates a persisted application receipt. */
  applicationBinding?: {
    receiptDigest: string;
    candidateDigest: string;
    observedSnapshotDigest: string;
  } | null;
  configurationDigest: string;
  oracleDigest: string;
  policyApproved: boolean;
  freshness: 'current' | 'stale' | 'unavailable';
  policies: {
    checkId: string;
    configurationDigest: string;
    requestDigest: string;
    profileId: string;
    language: string;
    architecturePolicyDigest: string | null;
    architecture: unknown;
    forbiddenEdges: { source: string; target: string }[];
  }[];
  observations: {
    checkId: string;
    configurationDigest: string;
    resultId: string;
    receiptId: string;
    graphDigest: string;
    status: 'pass' | 'fail';
    precision: 'cargo_resolved' | 'configured_build' | 'clang_semantic' | 'project_imports';
    findings: {
      ruleId: string;
      message: string;
      path: string[];
      locations: { path: string; contentDigest: string }[];
    }[];
  }[];
  diagnostics: string[];
}
export interface ArchitectureContext {
  root: string | null;
  projectId: string | null | undefined;
  chatProjectId: string | null;
  chatId: string | null;
  hostId: string | undefined;
  unsavedFiles: readonly string[];
}
export interface ArchitectureSelection {
  target: HarnessTarget;
  runId: string;
  label: string;
  projectId: string;
  hostId?: string;
}
export interface ArchitectureView {
  selection: ArchitectureSelection;
  feedback?: ArchitectureFeedback;
  state: 'loading' | 'current' | 'stale' | 'unavailable';
  reason?: string;
}
export interface ArchitectureFinding {
  key: string;
  checkId: string;
  ruleId: string;
  message: string;
  path?: string;
  chain: string[];
  contentDigest?: string;
  precision: string;
}
/** Editor URIs lowercase Windows drive letters; retain all other path characters. */
export function normalizedArchitecturePath(path: string): string {
  const value = path.replace(/\\/g, '/');
  return /^[A-Z]:\//.test(value) ? value[0].toLowerCase() + value.slice(1) : value;
}
/** Repository-relative paths only; backend workspace paths never select editor files. */
export function architectureFilePath(root: string, relative: string): string | undefined {
  if (
    !relative ||
    relative.includes('\\') ||
    relative.startsWith('/') ||
    /^[a-z]:/i.test(relative) ||
    relative.split('/').some((part) => !part || part === '.' || part === '..')
  )
    return;
  return `${root.replace(/[\\/]+$/, '')}/${relative}`;
}
/** Preserve file-level precision: graph artifacts do not supply source spans. */
export function architectureFindings(feedback: ArchitectureFeedback): ArchitectureFinding[] {
  return feedback.observations.flatMap((observation) =>
    observation.findings.flatMap((finding, index) => {
      const common = {
        checkId: observation.checkId,
        ruleId: finding.ruleId,
        message: finding.message,
        chain: finding.path,
        precision: observation.precision,
      };
      return finding.locations.length
        ? finding.locations.map((location, locationIndex) => ({
            ...common,
            key: `${observation.resultId}:${index}:${locationIndex}`,
            path: location.path,
            contentDigest: location.contentDigest,
          }))
        : [{ ...common, key: `${observation.resultId}:${index}` }];
    })
  );
}
/** Match source identities from the trusted host, including its authenticated application receipt. */
export function architectureSourceMatches(feedback: ArchitectureFeedback): boolean {
  const current = feedback.currentSnapshotDigest;
  const candidate = feedback.evidenceCandidateDigest;
  if (!current || !candidate) return false;
  if (current === candidate) return true;
  const binding = feedback.applicationBinding;
  return (
    !!binding?.receiptDigest &&
    binding.candidateDigest === candidate &&
    binding.observedSnapshotDigest === current
  );
}

/** Remote evidence has no whole-project correspondence with desktop buffers. */
const REMOTE_EVIDENCE_REASON =
  'This evidence describes the selected host’s saved project. It is historical in this editor because whole-project correspondence with the desktop has not been established. Editor markers and source navigation are unavailable.';

/** Current desktop markers require local source freshness and approved policy evidence. */
export function architectureMarkersCurrent(view: ArchitectureView | undefined): boolean {
  return (
    !!view &&
    !view.selection.target.dialogId &&
    view.state === 'current' &&
    view.feedback?.freshness === 'current' &&
    view.feedback.policyApproved &&
    view.feedback.observations.length > 0 &&
    architectureSourceMatches(view.feedback)
  );
}

/** Project-scoped, cancellable read state shared by the result action and editor. */
export function createArchitectureFeedbackController(
  request: (target: HarnessTarget, runId: string) => Promise<ArchitectureFeedback>
) {
  const [view, setView] = createSignal<ArchitectureView>();
  const listeners = new Set<(value: ArchitectureView | undefined) => void>();
  let context: ArchitectureContext = {
    root: null,
    projectId: null,
    chatProjectId: null,
    chatId: null,
    hostId: undefined,
    unsavedFiles: [],
  };
  let version = 0;
  const publish = (value: ArchitectureView | undefined) => {
    setView(value);
    for (const listener of listeners) listener(value);
  };
  function contextProblem(selection: ArchitectureSelection): string | undefined {
    if (
      context.root !== selection.target.root ||
      !context.projectId ||
      context.projectId !== selection.projectId
    )
      return 'The selected project changed. Choose architecture feedback from this project’s verification result.';
    if (
      selection.target.dialogId &&
      (context.chatProjectId !== context.projectId ||
        context.chatId !== selection.target.dialogId ||
        !context.hostId ||
        context.hostId !== selection.hostId)
    )
      return 'The selected chat or execution host changed. Select its current verification result again.';
    if (context.unsavedFiles.length)
      return 'Unsaved editor changes invalidate architecture evidence. Save or undo the edits, then refresh.';
  }
  function invalidate(reason: string) {
    version++;
    const current = view();
    if (current) publish({ ...current, state: 'stale', reason });
  }
  async function refresh() {
    const current = view();
    if (!current) return;
    const problem = contextProblem(current.selection);
    if (problem) {
      invalidate(problem);
      return;
    }
    const number = ++version;
    publish({ ...current, state: 'loading', reason: undefined });
    try {
      const feedback = await request(current.selection.target, current.selection.runId);
      if (feedback.runId !== current.selection.runId)
        throw new Error('The returned architecture evidence belongs to a different run.');
      if (number !== version) return;
      const changed = contextProblem(current.selection);
      if (changed) {
        invalidate(changed);
        return;
      }
      const hasMatchingSource = architectureSourceMatches(feedback);
      const remote = !!current.selection.target.dialogId;
      const state =
        feedback.freshness === 'current' &&
        (!feedback.policyApproved || !hasMatchingSource || !feedback.observations.length)
          ? 'unavailable'
          : remote && feedback.freshness === 'current'
            ? 'stale'
            : feedback.freshness;
      publish({
        selection: current.selection,
        feedback,
        state,
        reason: remote
          ? REMOTE_EVIDENCE_REASON
          : state === 'unavailable'
            ? 'Current approved architecture evidence could not be established.'
            : state === 'stale'
              ? 'Architecture evidence belongs to a different source snapshot or configuration.'
              : undefined,
      });
    } catch (error) {
      if (number === version)
        publish({
          ...current,
          state: 'unavailable',
          reason: `Architecture feedback could not be read: ${String(error)}`,
        });
    }
  }
  return {
    view,
    subscribe(listener: (value: ArchitectureView | undefined) => void) {
      listeners.add(listener);
      listener(view());
      return () => listeners.delete(listener);
    },
    updateContext(next: ArchitectureContext) {
      const previous = context;
      context = next;
      if (previous.root !== next.root || previous.projectId !== next.projectId) {
        version++;
        publish(undefined);
        return;
      }
      const current = view();
      if (current) {
        const problem = contextProblem(current.selection);
        if (problem) invalidate(problem);
        else publish({ ...current });
      }
    },
    async show(target: HarnessTarget, runId: string, label: string) {
      version++;
      if (!context.projectId || target.root !== context.root) return;
      const selection: ArchitectureSelection = {
        target: { ...target },
        runId,
        label,
        projectId: context.projectId,
        hostId: target.dialogId ? context.hostId : undefined,
      };
      publish({ selection, state: 'loading' });
      await refresh();
    },
    refresh,
    canRefresh() {
      const current = view();
      return !!current && !contextProblem(current.selection) && current.state !== 'loading';
    },
    invalidate,
    sourceChanged(path: string) {
      const root = context.root
        ? normalizedArchitecturePath(context.root).replace(/\/$/, '')
        : undefined;
      const file = normalizedArchitecturePath(path);
      if (root && (file === root || file.startsWith(`${root}/`)))
        invalidate('Source files changed. Refresh architecture feedback for the saved project.');
    },
    inputsChanged(target: HarnessTarget) {
      const current = view();
      if (
        current &&
        current.selection.target.root === target.root &&
        (current.selection.target.dialogId ?? null) === (target.dialogId ?? null)
      )
        invalidate('Verification configuration changed. Refresh architecture feedback.');
    },
    dismiss() {
      version++;
      publish(undefined);
    },
  };
}
export const architectureFeedback = createArchitectureFeedbackController((target, runId) =>
  harnessRequest(target, { action: 'architectureFeedback', runId })
);
