import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { SourceChangeQueue, type SourceChangeSink } from './sourceChanges';
import { QuietRunner } from './quietRunner';

function recorder() {
  const applied: Map<string, string | null | undefined>[] = [];
  let reloads = 0;
  const sink: SourceChangeSink = {
    apply: async (changes) => void applied.push(new Map(changes)),
    reload: async () => void reloads++,
  };
  return { sink, applied, reloads: () => reloads };
}

const timing = { quietMs: 10, maxWaitMs: 60, burstLimit: 2, reloadQuietMs: 20 };

describe('SourceChangeQueue', () => {
  it('applies changes together once they settle', (t) => {
    t.mock.timers.enable({ apis: ['setTimeout', 'Date'], now: 0 });
    const r = recorder();
    const q = new SourceChangeQueue(r.sink, timing);
    t.after(() => q.dispose());
    q.queue('/p/a.ts', 'A');
    q.queue('/p/b.ts');
    q.queue('/p/a.ts', null);
    t.mock.timers.tick(timing.quietMs - 1);
    assert.equal(r.applied.length, 0);
    t.mock.timers.tick(1);
    assert.equal(r.applied.length, 1);
    assert.deepEqual(
      [...r.applied[0]],
      [
        ['/p/a.ts', null],
        ['/p/b.ts', undefined],
      ]
    );
    assert.equal(r.reloads(), 0);
  });

  it('flushes by maxWaitMs while changes keep coming', (t) => {
    t.mock.timers.enable({ apis: ['setTimeout', 'Date'], now: 0 });
    const r = recorder();
    const q = new SourceChangeQueue(r.sink, timing);
    t.after(() => q.dispose());
    for (let elapsed = 0; elapsed < timing.maxWaitMs; elapsed += 4) {
      assert.equal(r.applied.length, 0, `no flush before the maximum wait at ${elapsed} ms`);
      q.queue('/p/a.ts', String(elapsed));
      t.mock.timers.tick(4);
    }
    assert.equal(r.applied.length, 1);
    assert.equal(r.applied[0].get('/p/a.ts'), '56');
  });

  it('a burst reloads everything instead, once quiet', (t) => {
    t.mock.timers.enable({ apis: ['setTimeout', 'Date'], now: 0 });
    const r = recorder();
    const q = new SourceChangeQueue(r.sink, timing);
    t.after(() => q.dispose());
    for (const f of ['a', 'b', 'c']) q.queue(`/p/${f}.ts`);
    t.mock.timers.tick(timing.quietMs);
    assert.equal(r.applied.length, 0);
    t.mock.timers.tick(timing.reloadQuietMs - 1);
    assert.equal(r.reloads(), 0);
    q.queue('/p/d.ts'); // while stale: only pushes the reload back
    t.mock.timers.tick(timing.reloadQuietMs - 1);
    assert.equal(r.reloads(), 0);
    t.mock.timers.tick(1);
    assert.equal(r.reloads(), 1);
    assert.equal(r.applied.length, 0);
    q.queue('/p/e.ts', 'E'); // no longer stale: applied as usual
    t.mock.timers.tick(timing.quietMs);
    assert.equal(r.applied.length, 1);
  });

  it('reloadAll: what was queued waits for the reload, which comes once quiet', (t) => {
    t.mock.timers.enable({ apis: ['setTimeout', 'Date'], now: 0 });
    const r = recorder();
    const q = new SourceChangeQueue(r.sink, timing);
    t.after(() => q.dispose());
    q.queue('/p/a.ts');
    q.reloadAll();
    t.mock.timers.tick(timing.reloadQuietMs - 1);
    assert.equal(r.reloads(), 0);
    q.queue('/p/b.ts'); // pushes the reload back
    t.mock.timers.tick(timing.reloadQuietMs - 1);
    assert.equal(r.reloads(), 0);
    t.mock.timers.tick(1);
    assert.equal(r.reloads(), 1);
    assert.equal(r.applied.length, 0, 'the reload has them');
  });
});

describe('QuietRunner', () => {
  it('runs once after the last trigger, and again if triggered while running', async (t) => {
    t.mock.timers.enable({ apis: ['setTimeout'] });
    let runs = 0;
    let release!: () => void;
    const blocked = new Promise<void>((resolve) => (release = resolve));
    const runner = new QuietRunner(() => {
      runs++;
      return runs === 1 ? blocked : Promise.resolve();
    }, 5);
    t.after(() => runner.dispose());
    runner.trigger();
    runner.trigger();
    t.mock.timers.tick(4);
    assert.equal(runs, 0);
    t.mock.timers.tick(1);
    assert.equal(runs, 1);
    runner.trigger(); // during the first run
    t.mock.timers.tick(5);
    assert.equal(runs, 1);
    release();
    // The runner subscribed first: its finally schedules the retry before this continuation.
    await blocked;
    t.mock.timers.tick(4);
    assert.equal(runs, 1);
    t.mock.timers.tick(1);
    assert.equal(runs, 2);
  });
});
