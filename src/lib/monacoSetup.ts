// monacoSetup.ts
//
// Monaco's web workers via Vite's `?worker` imports. The previous version
// built worker URLs by hand (`new URL('../../node_modules/...', import.meta.url)`),
// which Vite doesn't bundle: in dev it happened to resolve through node_modules,
// in a Tauri production build (file://, hashed chunk names) the TS/json/css/html
// workers 404'd and IntelliSense silently died while basic coloring kept working.
// Themes only exposed it — switching a theme triggers a re-highlight, the first
// thing that needs a working editor worker.
//
// `?worker` makes Vite emit each worker as its own chunk with the right URL in
// both `vite dev` and `tauri build`. Paths go through the package's `exports`
// map (`./*.js` → `./esm/vs/*.js`): `monaco-editor/esm/...` paths bypass it
// and fail to resolve, so `monaco-editor/editor/editor.worker.js` form is used.
import EditorWorker from 'monaco-editor/editor/editor.worker.js?worker';
import TsWorker from 'monaco-editor/language/typescript/ts.worker.js?worker';
import JsonWorker from 'monaco-editor/language/json/json.worker.js?worker';
import CssWorker from 'monaco-editor/language/css/css.worker.js?worker';
import HtmlWorker from 'monaco-editor/language/html/html.worker.js?worker';

type ViteWorkerFactory = new (options?: { name?: string }) => Worker;

/**
 * Monaco's workers are ES modules, so each must run as a module worker
 * (microsoft/monaco-editor docs/integrate-esm.md, "Using Vite"):
 * `new Worker(url, { type: 'module' })`. Constructing one with no options
 * makes a classic worker, which throws on the workers' `import` statements —
 * the worker dies and every IntelliSense feature its language provides
 * (completions, hover, cross-file go-to-definition, diagnostics) silently
 * stops, while basic token coloring keeps working. Vite forwards the
 * constructor argument to the native Worker, so `type` reaches it even
 * though vite/client's `*?worker` types only declare `name`.
 */
function spawn(factory: ViteWorkerFactory, label: string): Worker {
  return new factory({ name: label, type: 'module' } as { name?: string });
}

self.MonacoEnvironment = {
  getWorker(_, label) {
    if (label === 'json') return spawn(JsonWorker, label);
    if (label === 'css' || label === 'less' || label === 'scss') return spawn(CssWorker, label);
    if (label === 'html' || label === 'handlebars' || label === 'razor')
      return spawn(HtmlWorker, label);
    if (label === 'typescript' || label === 'javascript') return spawn(TsWorker, label);
    return spawn(EditorWorker, label);
  },
};
