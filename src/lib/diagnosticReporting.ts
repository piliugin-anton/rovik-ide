/** Marker information forwarded to the backend's editor diagnostics cache. */
export interface EditorDiagnostic {
  startLine: number;
  startCol: number;
  endLine: number;
  endCol: number;
  severity: string;
  message: string;
  source?: string;
  code?: string;
}
export interface OwnedEditorDiagnostic extends EditorDiagnostic {
  owner: string;
}

/** Only TypeScript's owners depend on its active compiler program. */
export function currentEditorDiagnostics(
  markers: readonly OwnedEditorDiagnostic[],
  typesCurrent: boolean
): EditorDiagnostic[] {
  return markers
    .filter((marker) => typesCurrent || !['typescript', 'javascript'].includes(marker.owner))
    .map(({ owner: _owner, ...marker }) => marker);
}
function normalized(path: string): string {
  const value = path.replace(/\\/g, '/');
  return /^[A-Za-z]:\//.test(value) ? value[0].toLowerCase() + value.slice(1) : value;
}
/** Root changes fence queued reports and failures from earlier projects. */
export function createDiagnosticReporter(deps: {
  path: (uri: string) => string | undefined;
  markers: (uri: string) => OwnedEditorDiagnostic[];
  typesCurrent: (path: string) => boolean;
  send: (root: string, path: string, markers: EditorDiagnostic[]) => Promise<void>;
  delayMs?: number;
}) {
  let root: string | null = null;
  let generation = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const pending = new Set<string>();
  const lastSent = new Map<string, string>();
  const owns = (path: string) => {
    if (!root) return false;
    const base = normalized(root).replace(/\/+$/, '');
    const file = normalized(path);
    return file === base || file.startsWith(`${base}/`);
  };
  async function push(uri: string) {
    const path = deps.path(uri);
    if (!root || !path || !owns(path)) return;
    const scope = generation;
    const currentRoot = root;
    const markers = currentEditorDiagnostics(deps.markers(uri), deps.typesCurrent(path));
    const identity = JSON.stringify(markers);
    if (lastSent.get(uri) === identity) return;
    lastSent.set(uri, identity);
    try {
      await deps.send(currentRoot, path, markers);
    } catch {
      if (scope === generation && lastSent.get(uri) === identity) lastSent.delete(uri);
    }
  }
  function schedule() {
    if (!root || timer !== undefined || !pending.size) return;
    timer = setTimeout(() => {
      timer = undefined;
      const batch = [...pending].slice(0, 100);
      for (const uri of batch) pending.delete(uri);
      void Promise.all(batch.map(push));
      schedule();
    }, deps.delayMs ?? 400);
  }
  function changed(uris: readonly string[]) {
    if (!root) return;
    for (const uri of uris) pending.add(uri);
    schedule();
  }
  function reset() {
    generation++;
    if (timer !== undefined) clearTimeout(timer);
    timer = undefined;
    pending.clear();
    lastSent.clear();
  }
  return {
    changed,
    project(next: string | null, openUris: readonly string[]) {
      if (root !== next) {
        reset();
        root = next;
      }
      changed(openUris);
    },
    restored(openUris: readonly string[]) {
      reset();
      changed(openUris);
    },
    dispose() {
      reset();
      root = null;
    },
  };
}
