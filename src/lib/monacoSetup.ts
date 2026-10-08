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

self.MonacoEnvironment = {
  getWorker(_, label) {
    if (label === 'json') return new JsonWorker();
    if (label === 'css' || label === 'less' || label === 'scss') return new CssWorker();
    if (label === 'html' || label === 'handlebars' || label === 'razor') return new HtmlWorker();
    if (label === 'typescript' || label === 'javascript') return new TsWorker();
    return new EditorWorker();
  },
};

