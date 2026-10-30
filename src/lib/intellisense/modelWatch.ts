// modelWatch.ts
//
// The editor models the IntelliSense project listens to, for the imports
// typed into them (project.ts looks a new import's package up once typing
// pauses).
//
// A model's listeners go with the model: go to definition makes a model
// per file it peeks at and idle ones are disposed (models.ts), so a project
// that outlives them must not keep their listeners. This keeps each watched
// model's listeners with the model, and the editor's listener for models
// created later with the project, so both go at the right time.
import type * as monaco from 'monaco-editor';

/** A model's events, as the watch uses them (Monaco's, or a test's). */
export interface WatchableModel {
  onDidChangeContent(listener: () => void): monaco.IDisposable;
  onWillDispose(listener: () => void): monaco.IDisposable;
}

/** The editor's models, as the watch uses them (Monaco's, or a test's). */
export interface EditorModels<M extends WatchableModel> {
  /** The models there are now. */
  all(): readonly M[];
  /** Calls `listen` for each model created from now on. */
  onDidCreateModel(listen: (model: M) => void): monaco.IDisposable;
}

/** What the watch does with the models it watches. */
export interface ModelWatchHooks<M extends WatchableModel> {
  /** Whether a model is one to watch (a source model of the project). */
  wants(model: M): boolean;
  /** A watched model's content changed. */
  edited(model: M): void;
  /** A watched model is disposed: what is held for it can go too. */
  disposed(model: M): void;
}

/**
 * Watches the editor's models that `hooks.wants` takes, those there now and
 * those created later, and reports their edits. Each model's listeners are
 * disposed and forgotten when the model is; the listener for models created
 * later stays until `dispose`, which drops everything.
 */
export class ModelWatch<M extends WatchableModel> {
  /** Each watched model's listeners, kept with it so they go with it. */
  private readonly listeners = new Map<M, monaco.IDisposable[]>();
  private readonly created: monaco.IDisposable;

  constructor(
    private readonly hooks: ModelWatchHooks<M>,
    editor: EditorModels<M>
  ) {
    for (const model of editor.all()) this.watch(model);
    this.created = editor.onDidCreateModel((model) => this.watch(model));
  }

  /** Stops watching every model, the disposed ones already gone. */
  dispose(): void {
    for (const listeners of this.listeners.values()) this.drop(listeners);
    this.listeners.clear();
    this.created.dispose();
  }

  private watch(model: M): void {
    if (!this.hooks.wants(model) || this.listeners.has(model)) return;
    const listeners: monaco.IDisposable[] = [];
    listeners.push(
      model.onDidChangeContent(() => this.hooks.edited(model)),
      // The model is going: its listeners with it, and whatever the hooks
      // hold for it.
      model.onWillDispose(() => {
        this.listeners.delete(model);
        this.drop(listeners);
        this.hooks.disposed(model);
      })
    );
    this.listeners.set(model, listeners);
  }

  private drop(listeners: monaco.IDisposable[]): void {
    for (const listener of listeners) listener.dispose();
  }
}
