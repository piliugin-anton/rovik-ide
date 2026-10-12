import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { SourceChangeQueue, type SourceChangeSink } from './sourceChanges';
import { QuietRunner } from './quietRunner';

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

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
  it('applies changes together once they settle', async () => {
    const r = recorder();
    const q = new SourceChangeQueue(r.sink, timing);
    q.queue('/p/a.ts', 'A');
    q.queue('/p/b.ts');
    q.queue('/p/a.ts', null);
    await sleep(30);
    assert.equal(r.applied.length, 1);
    assert.deepEqual(
      [...r.applied[0]],
      [
        ['/p/a.ts', null],
        ['/p/b.ts', undefined],
      ]
    );
    assert.equal(r.reloads(), 0);
    q.dispose();
  });

  it('flushes by maxWaitMs while changes keep coming', async () => {
    const r = recorder();
    const q = new SourceChangeQueue(r.sink, timing);
    const start = Date.now();
    while (Date.now() - start < 100 && r.applied.length === 0) {
      q.queue('/p/a.ts', String(Date.now()));
      await sleep(4);
    }
    assert.equal(r.applied.length, 1);
    q.dispose();
  });

  it('a burst reloads everything instead, once quiet', async () => {
    const r = recorder();
    const q = new SourceChangeQueue(r.sink, timing);
    for (const f of ['a', 'b', 'c']) q.queue(`/p/${f}.ts`);
    await sleep(15);
    assert.equal(r.applied.length, 0);
    q.queue('/p/d.ts'); // while stale: only pushes the reload back
    await sleep(15);
    assert.equal(r.reloads(), 0);
    await sleep(15);
    assert.equal(r.reloads(), 1);
    assert.equal(r.applied.length, 0);
    q.queue('/p/e.ts', 'E'); // no longer stale: applied as usual
    await sleep(20);
    assert.equal(r.applied.length, 1);
    q.dispose();
  });
});

describe('QuietRunner', () => {
  it('runs once after the last trigger, and again if triggered while running', async () => {
    let runs = 0;
    let release!: () => void;
    const runner = new QuietRunner(async () => {
      runs++;
      if (runs === 1) await new Promise<void>((r) => (release = r));
    }, 5);
    runner.trigger();
    runner.trigger();
    await sleep(10);
    assert.equal(runs, 1);
    runner.trigger(); // during the first run
    await sleep(10);
    assert.equal(runs, 1);
    release();
    await sleep(15);
    assert.equal(runs, 2);
    runner.dispose();
  });
});
