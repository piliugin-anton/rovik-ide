import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { ModelWatch, type EditorModels, type WatchableModel } from './modelWatch';

/** A listener Monaco would hand out: disposing it unregisters it, as Monaco does. */
class Sub {
  disposed = false;

  constructor(
    private readonly live: Set<Sub>,
    private readonly unregister: () => void
  ) {
    live.add(this);
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.live.delete(this);
    this.unregister();
  }
}

/** A model that reports its own listeners, as Monaco's does. */
class FakeModel implements WatchableModel {
  /** Its listeners, null where one was disposed. */
  private readonly edits: ((() => void) | null)[] = [];
  private readonly going: ((() => void) | null)[] = [];

  constructor(
    readonly path: string,
    private readonly live: Set<Sub>
  ) {}

  onDidChangeContent(listener: () => void): Sub {
    const at = this.edits.push(listener) - 1;
    return new Sub(this.live, () => (this.edits[at] = null));
  }

  onWillDispose(listener: () => void): Sub {
    const at = this.going.push(listener) - 1;
    return new Sub(this.live, () => (this.going[at] = null));
  }

  /** Typed into. */
  edit(): void {
    for (const listener of this.edits) listener?.();
  }

  /** Disposed by the editor. */
  dispose(): void {
    for (const listener of [...this.going]) listener?.();
    this.edits.fill(null);
    this.going.fill(null);
  }
}

class FakeEditor implements EditorModels<FakeModel> {
  /** Every listener handed out and not disposed. */
  readonly live = new Set<Sub>();
  readonly models: FakeModel[] = [];
  private readonly created: (((model: FakeModel) => void) | null)[] = [];

  all(): readonly FakeModel[] {
    return this.models;
  }

  onDidCreateModel(listen: (model: FakeModel) => void): Sub {
    const at = this.created.push(listen) - 1;
    return new Sub(this.live, () => (this.created[at] = null));
  }

  /** A model the editor makes. */
  create(path: string): FakeModel {
    const model = new FakeModel(path, this.live);
    this.models.push(model);
    for (const listener of this.created) listener?.(model);
    return model;
  }
}

describe('watching the editor models', () => {
  it('keeps no listener of a disposed model, over a thousand models', () => {
    const editor = new FakeEditor();
    const watch = new ModelWatch<FakeModel>(hooks(), editor);
    for (let i = 0; i < 1_000; i++) editor.create(`/p/file${i}.ts`).dispose();
    // Disposed subscriptions can still retain models: check ownership, not just live listeners.
    assert.equal(watch['listeners'].size, 0, 'the watcher forgets every disposed model');
    assert.equal(editor.live.size, 1, "only the editor's listener for new models is left");
    watch.dispose();
    assert.equal(editor.live.size, 0, 'the project takes the last one with it');
  });

  it('watches existing source models and forgets them when they are disposed', () => {
    const editor = new FakeEditor();
    const existing = editor.create('/p/existing.ts');
    const ignored = editor.create('/p/notes.md');
    const seen: string[] = [];
    const disposed: string[] = [];
    const watch = new ModelWatch<FakeModel>(hooks(seen, disposed), editor);
    assert.equal(watch['listeners'].size, 1);
    assert.equal(editor.live.size, 3, 'two model listeners and the editor creation listener');
    existing.edit();
    ignored.edit();
    assert.deepEqual(seen, ['/p/existing.ts']);
    existing.dispose();
    ignored.dispose();
    assert.deepEqual(disposed, ['/p/existing.ts']);
    assert.equal(watch['listeners'].size, 0);
    assert.equal(editor.live.size, 1);
    watch.dispose();
    assert.equal(editor.live.size, 0);
  });

  it('reports the edits of the models it watches, and only those', () => {
    const editor = new FakeEditor();
    const seen: string[] = [];
    new ModelWatch<FakeModel>(hooks(seen), editor);
    const before = editor.create('/p/early.ts');
    before.edit();
    const later = editor.create('/p/later.ts');
    later.edit();
    editor.create('/p/notes.md').edit();
    assert.deepEqual(seen, ['/p/early.ts', '/p/later.ts']);
  });

  it('says when a watched model is disposed, once', () => {
    const editor = new FakeEditor();
    const disposed: string[] = [];
    new ModelWatch<FakeModel>(hooks([], disposed), editor);
    editor.create('/p/a.ts').dispose();
    editor.create('/p/b.md').dispose();
    assert.deepEqual(disposed, ['/p/a.ts']);
  });

  it('stops watching every model when it is disposed', () => {
    const editor = new FakeEditor();
    const seen: string[] = [];
    const watch = new ModelWatch<FakeModel>(hooks(seen), editor);
    const open = editor.create('/p/open.ts');
    assert.equal(watch['listeners'].size, 1);
    watch.dispose();
    assert.equal(watch['listeners'].size, 0, 'teardown releases ownership of open models');
    open.edit();
    editor.create('/p/after.ts').edit();
    assert.deepEqual(seen, []);
    assert.equal(editor.live.size, 0);
  });
});

/** Source models only, as the project takes them (`.ts` here). */
function hooks(edited: string[] = [], disposed: string[] = []) {
  return {
    wants: (model: FakeModel) => model.path.endsWith('.ts'),
    edited: (model: FakeModel) => void edited.push(model.path),
    disposed: (model: FakeModel) => void disposed.push(model.path),
  };
}
