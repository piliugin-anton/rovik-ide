import * as monaco from 'monaco-editor';
import { architectureFeedback } from './architectureFeedback';
import { installArchitectureMarkers, type ArchitectureModel } from './architectureMarkers';

/** Connect cached file models to the advisory architecture marker owner. */
export function startArchitectureMarkers(): () => void {
  const native = new Map<ArchitectureModel, monaco.editor.ITextModel>();
  const wrappers = new WeakMap<monaco.editor.ITextModel, ArchitectureModel>();
  function wrap(model: monaco.editor.ITextModel): ArchitectureModel {
    const existing = wrappers.get(model);
    if (existing) return existing;
    const value: ArchitectureModel = {
      path: model.uri.scheme === 'file' ? model.uri.fsPath : '',
      disposed: () => model.isDisposed(),
      content: () => model.getValue(monaco.editor.EndOfLinePreference.TextDefined, true),
      version: () => model.getVersionId(),
      changed(listener) {
        const subscription = model.onDidChangeContent(listener);
        return () => subscription.dispose();
      },
    };
    wrappers.set(model, value);
    native.set(value, model);
    return value;
  }
  const dispose = installArchitectureMarkers(architectureFeedback, {
    models: () =>
      monaco.editor
        .getModels()
        .filter((model) => model.uri.scheme === 'file')
        .map(wrap),
    created(listener) {
      const subscription = monaco.editor.onDidCreateModel((model) => {
        if (model.uri.scheme === 'file') listener(wrap(model));
      });
      return () => subscription.dispose();
    },
    disposing(listener) {
      const subscription = monaco.editor.onWillDisposeModel((model) => {
        const value = wrappers.get(model);
        if (value) {
          listener(value);
          native.delete(value);
        }
      });
      return () => subscription.dispose();
    },
    publish(model, owner, markers) {
      const value = native.get(model);
      if (value && !value.isDisposed()) monaco.editor.setModelMarkers(value, owner, markers);
    },
  });
  return () => {
    dispose();
    native.clear();
  };
}
