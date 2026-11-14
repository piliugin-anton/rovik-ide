import type {
  ArchitectureFinding,
  ArchitectureView,
  createArchitectureFeedbackController,
} from './architectureFeedback';
import {
  architectureFilePath,
  architectureFindings,
  architectureMarkersCurrent,
  normalizedArchitecturePath,
} from './architectureFeedback';

/** This owner never clears TypeScript, rust-analyzer or another extension's markers. */
export const ARCHITECTURE_MARKER_OWNER = 'rovik-polyglot-architecture';
export interface ArchitectureMarker {
  severity: number;
  message: string;
  source: string;
  code: string;
  startLineNumber: number;
  startColumn: number;
  endLineNumber: number;
  endColumn: number;
}
/** Monaco requires an anchor; the message explicitly says it is not an observed source span. */
export function architectureMarker(finding: ArchitectureFinding): ArchitectureMarker {
  return {
    severity: 4,
    message: `[File-level architecture finding; precise source location unavailable] ${finding.message}${finding.chain.length ? `\nDependency path: ${finding.chain.join(' → ')}` : ''}`,
    source: 'Polyglot architecture (advisory)',
    code: finding.ruleId,
    startLineNumber: 1,
    startColumn: 1,
    endLineNumber: 1,
    endColumn: 1,
  };
}
export interface ArchitectureModel {
  path: string;
  disposed(): boolean;
  content(): string;
  version(): number;
  changed(listener: () => void): () => void;
}
export interface ArchitectureMarkerHost {
  models(): ArchitectureModel[];
  created(listener: (model: ArchitectureModel) => void): () => void;
  disposing(listener: (model: ArchitectureModel) => void): () => void;
  publish(model: ArchitectureModel, owner: string, markers: ArchitectureMarker[]): void;
}
/** Exact UTF-8 bytes, retaining the model's line endings and optional BOM. */
export async function architectureContentDigest(content: string): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(content));
  return `sha256:${Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join('')}`;
}

/** Model creation, cache eviction, edits and project changes share one dedicated owner. */
export function installArchitectureMarkers(
  controller: ReturnType<typeof createArchitectureFeedbackController>,
  host: ArchitectureMarkerHost,
  digest: (content: string) => Promise<string> = architectureContentDigest
): () => void {
  let current: ArchitectureView | undefined;
  let publication = 0;
  const watched = new Map<ArchitectureModel, () => void>();
  async function apply(model: ArchitectureModel) {
    if (model.disposed()) return;
    const findings = architectureMarkersCurrent(current)
      ? architectureFindings(current!.feedback!).filter(
          (finding) =>
            finding.path &&
            normalizedArchitecturePath(
              architectureFilePath(current!.selection.target.root, finding.path) ?? ''
            ) === normalizedArchitecturePath(model.path)
        )
      : [];
    host.publish(model, ARCHITECTURE_MARKER_OWNER, []);
    if (!findings.length) return;
    const sequence = publication;
    const version = model.version();
    try {
      const identity = await digest(model.content());
      if (
        sequence !== publication ||
        model.disposed() ||
        !watched.has(model) ||
        version !== model.version()
      )
        return;
      if (findings.some((finding) => finding.contentDigest !== identity)) {
        controller.invalidate(
          `The displayed buffer for ${model.path} differs from the saved architecture evidence. Reload or save the file, then refresh.`
        );
        return;
      }
      host.publish(model, ARCHITECTURE_MARKER_OWNER, findings.map(architectureMarker));
    } catch (error) {
      if (sequence === publication && watched.has(model) && !model.disposed())
        controller.invalidate(`The displayed source bytes could not be checked: ${String(error)}`);
    }
  }
  function watch(model: ArchitectureModel) {
    if (!watched.has(model))
      watched.set(
        model,
        model.changed(() => controller.sourceChanged(model.path))
      );
    void apply(model);
  }
  const stopCreate = host.created(watch);
  const stopDispose = host.disposing((model) => {
    watched.get(model)?.();
    watched.delete(model);
  });
  for (const model of host.models()) watch(model);
  const unsubscribe = controller.subscribe((view) => {
    current = view;
    publication++;
    for (const model of host.models()) void apply(model);
  });
  return () => {
    publication++;
    stopCreate();
    stopDispose();
    unsubscribe();
    for (const stop of watched.values()) stop();
    watched.clear();
    for (const model of host.models())
      if (!model.disposed()) host.publish(model, ARCHITECTURE_MARKER_OWNER, []);
  };
}
