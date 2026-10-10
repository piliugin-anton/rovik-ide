// @ts-nocheck
// todo.ts checklist tests (node:test). Run with: node --test src/lib/todo.test.ts
// Frontend parsing only: latestChecklist covers the current request; carry
// across follow-ups lives in the backend (plan::current_checklist).
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { isSettled, latestChecklist, openSteps, parseTodo } from './todo';
import type { ChatMessage } from '../types';

function msg(partial) {
  return {
    role: 'assistant',
    content: '',
    ...partial,
  } as ChatMessage;
}

function todoMsg(id, args, extra) {
  return msg({
    id,
    role: 'assistant',
    toolCalls: [{ id: `${id}-call`, name: 'todo', arguments: JSON.stringify(args) }],
    ...extra,
  });
}

describe('todo checklist parsing', () => {
  it('unfinished steps stay open, settled ones are done/skipped/changed', () => {
    assert.equal(isSettled({ title: 'a', status: 'pending' }), false);
    assert.equal(isSettled({ title: 'a', status: 'in_progress' }), false);
    assert.equal(isSettled({ title: 'a', status: 'done' }), true);
    assert.equal(isSettled({ title: 'a', status: 'skipped' }), true);
    assert.equal(isSettled({ title: 'a', status: 'changed' }), true);
    const items = parseTodo({ items: ['a', { title: 'b', status: 'done' }] });
    assert.equal(openSteps(items).length, 1);
  });

  it('a newer todo call in the current request wins', () => {
    const messages = [
      msg({ id: 'u1', role: 'user' }),
      todoMsg('a1', { items: ['old step'] }),
      todoMsg('a2', { items: ['new step'] }),
    ];
    assert.deepEqual(
      latestChecklist(messages)?.map((i) => i.title),
      ['new step']
    );
  });

  it('the current request stops at the last user message', () => {
    const messages = [
      msg({ id: 'u1', role: 'user' }),
      todoMsg('a1', { items: ['old step'] }),
      msg({ id: 'u2', role: 'user' }),
    ];
    // Frontend parsing only: no carry across follow-ups here (the backend
    // `current_checklist`/`chat_checklist` does that over the whole DB).
    assert.equal(latestChecklist(messages), null);
  });

  it('a second user message also stops the search', () => {
    const messages = [
      msg({ id: 'u1', role: 'user' }),
      todoMsg('a1', { items: ['old step'] }),
      msg({ id: 'u2', role: 'user' }),
      msg({ id: 'u3', role: 'user' }),
    ];
    assert.equal(latestChecklist(messages), null);
  });

  it('a still-streaming (partial JSON) call falls back to the older call', () => {
    const m = msg({
      id: 'a1',
      role: 'assistant',
      toolCalls: [
        { id: 'old', name: 'todo', arguments: JSON.stringify({ items: ['old step'] }) },
        { id: 'new', name: 'todo', arguments: '{"items": ["new' },
      ],
    });
    assert.deepEqual(
      latestChecklist([msg({ id: 'u1', role: 'user' }), m])?.map((i) => i.title),
      ['old step']
    );
  });
});
