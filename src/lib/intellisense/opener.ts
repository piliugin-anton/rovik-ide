// opener.ts
//
// Go to definition in another file. Monaco can't open other files by
// itself: an editor opener hands the file to the app's tabs
// (lib/openInEditor.ts), at the definition's line.
import * as monaco from 'monaco-editor';
import { openInEditor } from '../openInEditor';

export function registerDefinitionOpener(currentRoot: () => string | null): void {
  monaco.editor.registerEditorOpener({
    openCodeEditor(source, resource, selectionOrPosition) {
      if (resource.scheme !== 'file' || !currentRoot()) return false;
      // In the same file Monaco moves the cursor itself (and keeps its
      // back/forward history): only other files need opening.
      if (source.getModel()?.uri.toString() === resource.toString()) return false;
      const line = !selectionOrPosition
        ? 1
        : 'startLineNumber' in selectionOrPosition
          ? selectionOrPosition.startLineNumber
          : selectionOrPosition.lineNumber;
      void openInEditor({ path: resource.fsPath, focus: true, at: { line } });
      return true;
    },
  });
}
