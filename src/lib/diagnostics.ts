// All Monaco owners report through one listener. Only TypeScript markers depend
// on its active compiler program; architecture and language-server owners do not.
import { onBackendRestored } from './backendSession';
import * as monaco from 'monaco-editor';
import { invoke } from '@tauri-apps/api/core';
import { typesScopeCovers } from './intellisense';
import { createDiagnosticReporter } from './diagnosticReporting';

let registered = false;
const openUris = () => monaco.editor.getModels().map((model) => model.uri.toString());
const reporter = createDiagnosticReporter({
  path(value) {
    const uri = monaco.Uri.parse(value);
    return uri.scheme === 'file'
      ? ((uri as unknown as { fsPath?: string }).fsPath ?? uri.path)
      : undefined;
  },
  markers(value) {
    return monaco.editor.getModelMarkers({ resource: monaco.Uri.parse(value) }).map((marker) => {
      const code =
        marker.code === undefined || marker.code === null
          ? undefined
          : typeof marker.code === 'object'
            ? String(marker.code.value ?? '')
            : String(marker.code);
      return {
        owner: marker.owner,
        startLine: marker.startLineNumber,
        startCol: marker.startColumn,
        endLine: marker.endLineNumber,
        endCol: marker.endColumn,
        severity: monaco.MarkerSeverity[marker.severity]?.toLowerCase() ?? 'info',
        message: marker.message,
        source: marker.source,
        code: code || undefined,
      };
    });
  },
  typesCurrent: typesScopeCovers,
  send: (root, path, markers) => invoke('report_editor_diagnostics', { root, path, markers }),
});

/** Follow the selected project, including project close, and report all current model owners. */
export function ensureDiagnosticsReporting(projectDir: string | null): void {
  if (!registered) {
    registered = true;
    onBackendRestored(() => reporter.restored(openUris()));
    monaco.editor.onDidChangeMarkers((uris) => reporter.changed(uris.map((uri) => uri.toString())));
  }
  reporter.project(projectDir, openUris());
}
