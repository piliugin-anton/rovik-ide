// todo.ts
//
// The agent's checklist (the `todo` tool, Agent mode): what it's working on,
// shown read-only in the panel above the input (TodoPanel). Nobody edits it
// in the UI: it's the agent's own report of its progress. Each call writes
// the whole list, so the latest call in a run is the current state; there's
// no separate storage, the tool calls are the record.
import type { ChatMessage } from '../types';

export type TodoStatus = 'pending' | 'in_progress' | 'done' | 'skipped' | 'changed';

export interface TodoItem {
  title: string;
  status: TodoStatus;
  /** Why a step was skipped or changed. */
  note?: string;
}

const STATUSES: TodoStatus[] = ['pending', 'in_progress', 'done', 'skipped', 'changed'];
const MAX_ITEMS = 30;

export function parseTodo(args: Record<string, unknown>): TodoItem[] {
  const raw = Array.isArray(args.items) ? args.items : [];
  return raw
    .slice(0, MAX_ITEMS)
    .map((r): TodoItem | null => {
      if (typeof r === 'string') return r.trim() ? { title: r.trim(), status: 'pending' } : null;
      if (!r || typeof r !== 'object') return null;
      const o = r as Record<string, unknown>;
      const title = typeof o.title === 'string' ? o.title.trim() : '';
      if (!title) return null;
      const status = STATUSES.includes(o.status as TodoStatus)
        ? (o.status as TodoStatus)
        : 'pending';
      const note = typeof o.note === 'string' && o.note.trim() ? o.note.trim() : undefined;
      return { title, status, note };
    })
    .filter((i): i is TodoItem => i !== null);
}

/** Finished: done, or skipped/changed with the reason given. */
export const isSettled = (i: TodoItem) =>
  i.status === 'done' || i.status === 'skipped' || i.status === 'changed';

export function progressLabel(items: TodoItem[]): string {
  return `${items.filter(isSettled).length} of ${items.length} done`;
}

/** What the model reads after an update. */
export function describeTodo(items: TodoItem[]): string {
  if (items.length === 0) return 'Error: the checklist is empty. Pass items: [{ title, status }].';
  const current = items.find((i) => i.status === 'in_progress');
  return `Checklist updated: ${progressLabel(items)}${current ? `; now: ${current.title}` : ''}.`;
}

/**
 * The checklist of the current request in a chat: the latest todo call since
 * the user's last message (the IDE's own messages don't start a request).
 */
export function latestChecklist(messages: ChatMessage[]): TodoItem[] | null {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.role === 'user' && !m.meta?.harness) return null;
    const call = [...(m.toolCalls ?? [])].reverse().find((c) => c.name === 'todo');
    if (call) {
      try {
        const items = parseTodo(JSON.parse(call.arguments));
        if (items.length) return items;
      } catch {
        // still streaming, or malformed: keep looking further back
      }
    }
  }
  return null;
}

export const TODO_SCHEMA = {
  type: 'function',
  function: {
    name: 'todo',
    description:
      'Keep your checklist for this task, shown to the user as what you are doing. Use it for tasks with ' +
      'three or more distinct steps or that span several files; not for small tasks. Pass the whole list ' +
      'each time. Create it before you start, mark a step in_progress when you begin it and done when it is ' +
      'finished (one step in progress at a time). If a step turns out unnecessary or has to change, mark it ' +
      'skipped or changed with a short note saying why, instead of silently dropping it. Update it when a ' +
      'step starts or finishes, not after every action.',
    parameters: {
      type: 'object',
      properties: {
        items: {
          type: 'array',
          maxItems: MAX_ITEMS,
          items: {
            type: 'object',
            properties: {
              title: { type: 'string', description: 'The step, a few words.' },
              status: { type: 'string', enum: STATUSES },
              note: { type: 'string', description: 'For skipped or changed: why, one sentence.' },
            },
            required: ['title', 'status'],
          },
        },
      },
      required: ['items'],
    },
  },
};
