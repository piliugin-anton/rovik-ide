// @ts-nocheck
// toolArgs.ts tests (node:test). Run with: npm test
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { callStatus, isPartialWrite, lineArg, num, planTitle, str, todoSummary } from './toolArgs';

describe('str and num', () => {
  it('keep only their own type', () => {
    assert.equal(str('a'), 'a');
    assert.equal(str(1), undefined);
    assert.equal(str(null), undefined);
    assert.equal(num(2), 2);
    assert.equal(num('2'), undefined);
  });
});

describe('lineArg', () => {
  it('takes a line number as a number or a string, from 1 up', () => {
    assert.equal(lineArg(12), 12);
    assert.equal(lineArg('12'), 12);
    assert.equal(lineArg(3.7), 3);
    assert.equal(lineArg(0), undefined);
    assert.equal(lineArg(-1), undefined);
    assert.equal(lineArg(''), undefined);
    assert.equal(lineArg('  '), undefined);
    assert.equal(lineArg('x'), undefined);
    assert.equal(lineArg(null), undefined);
    assert.equal(lineArg(undefined), undefined);
  });
});

describe('isPartialWrite', () => {
  it('is a write with an old_text', () => {
    assert.equal(isPartialWrite('write', { old_text: 'a', content: 'b' }), true);
    assert.equal(isPartialWrite('write', { old_text: '', content: 'b' }), true);
    assert.equal(isPartialWrite('write', { old_text: null, content: 'b' }), false);
    assert.equal(isPartialWrite('write', { content: 'b' }), false);
    assert.equal(isPartialWrite('read', { old_text: 'a' }), false);
  });
});

describe('planTitle', () => {
  it('is the first heading, as written', () => {
    assert.equal(planTitle('Intro\n# Add caching  \n\nSteps'), 'Add caching  ');
    assert.equal(planTitle('## Not a title\ntext'), undefined);
    assert.equal(planTitle(''), undefined);
    assert.equal(planTitle(undefined), undefined);
  });
});

describe('todoSummary', () => {
  const items = [
    { title: 'One', status: 'done' },
    { title: 'Two is a rather long step title', status: 'in_progress' },
    { title: 'Three', status: 'pending' },
  ];
  it('counts the steps done and names the one in progress', () => {
    assert.equal(todoSummary({ items }), '1 of 3 done · Two is a rather long step title');
    assert.equal(todoSummary({ items: items.slice(0, 1) }), '1 of 1 done');
    assert.equal(todoSummary({ items: [] }), undefined);
    assert.equal(todoSummary({}), undefined);
  });
  it('shortens the title as asked', () => {
    assert.equal(
      todoSummary({ items }, (t) => t.slice(0, 3)),
      '1 of 3 done · Two'
    );
  });
});

describe('callStatus', () => {
  it('follows the result, then the run', () => {
    assert.equal(callStatus(undefined, true), 'running');
    assert.equal(callStatus(undefined, false), 'idle');
    assert.equal(callStatus(undefined, undefined), 'idle');
    assert.equal(callStatus({ content: 'ok' }, true), 'done');
    assert.equal(callStatus({ content: 'Error: no' }, false), 'error');
    assert.equal(callStatus({ content: '  Error: no' }, true), 'error');
  });
  it('never spins a superseded call', () => {
    assert.equal(callStatus(undefined, true, true), 'idle');
    assert.equal(callStatus({ content: 'ok' }, true, true), 'done');
  });
});
