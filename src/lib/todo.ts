// todo.ts
//
// The agent's checklist (the `todo` tool, Agent mode): what it's working on,
// shown read-only in the panel above the input (TodoPanel). Nobody edits it
// in the UI: it's the agent's own report of its progress. Each call writes
// the whole list, so the latest call in a run is the current state; there's
// no separate storage, the tool calls are the record.
import type { ChatMessage } from '../types';
import { isRequestMessage } from './requests';

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

/** Steps still open. */
export const openSteps = (items: TodoItem[]) => items.filter((i) => !isSettled(i));

export function progressLabel(items: TodoItem[]): string {
  return `${items.filter(isSettled).length} of ${items.length} done`;
}

/**
 * Try every `todo` call in one message, newest first: while a call streams
 * its arguments are partial JSON, so the newest call may not parse yet
 * while an older one in the same message still does.
 */
function todoInMessage(m: ChatMessage): TodoItem[] | null {
  const calls = (m.toolCalls ?? []).filter((c) => c.name === 'todo');
  for (let i = calls.length - 1; i >= 0; i--) {
    try {
      const items = parseTodo(JSON.parse(calls[i].arguments));
      if (items.length) return items;
    } catch {
      // still streaming, or malformed: try the next older call
    }
  }
  return null;
}

/**
 * The checklist of the current request in a chat: the latest todo call since
 * the user's last message (the IDE's own messages don't start a request, nor
 * do the user's sent while the run worked: lib/requests.ts).
 *
 * Parsing only: this stays to show a call while it streams. Everything else
 * uses the backend (`chat_checklist` when idle, the `checklist` event while
 * a run goes), which sees the whole database — not just the messages loaded
 * here — and carries open checklists across short follow-ups.
 */
export function latestChecklist(messages: ChatMessage[]): TodoItem[] | null {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (isRequestMessage(m)) return null;
    const items = todoInMessage(m);
    if (items) return items;
  }
  return null;
}
