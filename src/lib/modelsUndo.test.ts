import '../test/monacoDom';
import { after, test } from 'node:test';
import assert from 'node:assert/strict';
import * as monaco from 'monaco-editor';
import { replaceKeepingUndo } from './models';

after(() => (window as unknown as { happyDOM: { close(): Promise<void> } }).happyDOM.close());

test('repeated external reloads keep one undo step with only the first and last text', async () => {
  const initial = 'a'.repeat(65_536);
  const model = monaco.editor.createModel(initial, 'plaintext');
  try {
    for (let i = 0; i < 200; i++) {
      replaceKeepingUndo(model, String.fromCharCode(65 + (i % 26)).repeat(initial.length));
    }
    const latest = model.getValue();
    // Monaco's own accounting catches retained intermediate full-document versions.
    const service = (
      model as unknown as {
        _undoRedoService: {
          getElements(uri: monaco.Uri): { past: { heapSize(): number }[] };
        };
      }
    )._undoRedoService;
    const entries = service.getElements(model.uri).past;
    assert.equal(entries.length, 1);
    assert.ok(entries.reduce((sum, e) => sum + e.heapSize(), 0) < initial.length * 5);
    await model.undo();
    assert.equal(model.getValue(), initial);
    assert.equal(model.canUndo(), false);
    await model.redo();
    assert.equal(model.getValue(), latest);
  } finally {
    model.dispose();
  }
});

test('user edits separate reload groups and remain undoable', async () => {
  const model = monaco.editor.createModel('start', 'plaintext');
  try {
    model.pushEditOperations([], [{ range: model.getFullModelRange(), text: 'typed' }], () => null);
    replaceKeepingUndo(model, 'disk one');
    replaceKeepingUndo(model, 'disk two');
    model.pushEditOperations(
      [],
      [{ range: model.getFullModelRange(), text: 'typed again' }],
      () => null
    );
    replaceKeepingUndo(model, 'disk three');
    replaceKeepingUndo(model, 'disk four');
    for (const expected of ['typed again', 'disk two', 'typed', 'start']) {
      await model.undo();
      assert.equal(model.getValue(), expected);
    }
    assert.equal(model.canUndo(), false);
  } finally {
    model.dispose();
  }
});

test('undo and redo between reloads start a fresh reload group', async () => {
  const model = monaco.editor.createModel('start', 'plaintext');
  try {
    replaceKeepingUndo(model, 'first');
    await model.undo();
    await model.redo();
    replaceKeepingUndo(model, 'second');
    await model.undo();
    assert.equal(model.getValue(), 'first');
    await model.undo();
    assert.equal(model.getValue(), 'start');
  } finally {
    model.dispose();
  }
});

test('reload groups restore line endings and stay independent across models', async () => {
  const a = monaco.editor.createModel('a\nb', 'plaintext');
  const b = monaco.editor.createModel('x', 'plaintext');
  try {
    replaceKeepingUndo(a, 'c\r\nd');
    replaceKeepingUndo(b, 'y');
    replaceKeepingUndo(a, 'e\r\nf');
    replaceKeepingUndo(b, 'z');
    await a.undo();
    assert.equal(a.getValue(), 'a\nb');
    assert.equal(a.canUndo(), false);
    assert.equal(b.getValue(), 'z');
    await b.undo();
    assert.equal(b.getValue(), 'x');
    assert.equal(b.canUndo(), false);
    await a.redo();
    assert.equal(a.getValue(), 'e\r\nf');
  } finally {
    a.dispose();
    b.dispose();
  }
});
