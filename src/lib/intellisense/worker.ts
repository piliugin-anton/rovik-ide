// worker.ts
//
// Entry of the TypeScript worker (lib/monacoSetup.ts spawns it): Monaco's
// bootstrap, with the project worker (projectWorker.ts) in place of its own.
import { initialize } from 'monaco-editor/language/typescript/ts.worker.js';
import { ProjectTypeScriptWorker } from './projectWorker';

self.onmessage = () => {
  initialize((ctx, createData) => new ProjectTypeScriptWorker(ctx, createData));
};
