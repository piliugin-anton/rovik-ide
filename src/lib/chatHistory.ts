import { invoke } from '@tauri-apps/api/core';
import type { ChatDialog } from '../types';

/*
 * Storage lives in src-tauri/src/chat_store.rs: one file per chat, written
 * atomically, damaged files quarantined rather than overwritten.
 */

export type LoadResult =
  | { ok: true; dialogs: ChatDialog[] }
  /** The store couldn't be read. Don't save anything this session. */
  | { ok: false; error: string };

function isDialog(value: unknown): value is ChatDialog {
  const d = value as ChatDialog;
  return (
    !!d &&
    typeof d === 'object' &&
    typeof d.id === 'string' &&
    typeof d.title === 'string' &&
    Array.isArray(d.messages)
  );
}

export async function loadChatHistory(): Promise<LoadResult> {
  try {
    const raw = await invoke<string>('load_chat_history');
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) {
      return { ok: false, error: 'Chat history has an unexpected format.' };
    }
    const dialogs = parsed.filter(isDialog).sort((a, b) => b.updatedAt - a.updatedAt);
    return { ok: true, dialogs };
  } catch (err) {
    console.error('Failed to load chat history:', err);
    return { ok: false, error: String(err) };
  }
}

function saveChatDialog(dialog: ChatDialog): Promise<void> {
  return invoke('save_chat_dialog', { id: dialog.id, json: JSON.stringify(dialog) });
}

function deleteChatDialog(id: string): Promise<void> {
  return invoke('delete_chat_dialog', { id });
}

/** Wait this long after the last change before saving... */
const IDLE_MS = 1000;
/** ...but never longer than this, so a crash mid-stream loses seconds, not the reply. */
const MAX_WAIT_MS = 5000;

export interface ChatSaver {
  /** Schedules a save of one chat; repeated calls coalesce. */
  markDirty(id: string): void;
  /** Saves everything pending now; resolves once it's on disk. */
  flush(): Promise<void>;
  /** Deletes a chat's file, after any pending writes. */
  remove(id: string): Promise<void>;
  /** Stops all writes for this session (used when loading failed). */
  disable(): void;
}

/**
 * Saves only the chats that changed. Writes go through one queue, so an
 * older snapshot can never land on disk after a newer one, and each write
 * serializes the chat as it is at that moment (the latest content).
 */
export function createChatSaver(
  getDialog: (id: string) => ChatDialog | undefined,
  io: { save: (dialog: ChatDialog) => Promise<void>; remove: (id: string) => Promise<void> } = {
    save: saveChatDialog,
    remove: deleteChatDialog,
  }
): ChatSaver {
  const dirty = new Set<string>();
  let timer: number | undefined;
  let firstDirtyAt = 0;
  let queue: Promise<void> = Promise.resolve();
  let enabled = true;

  const enqueue = (task: () => Promise<void>) => {
    queue = queue.then(task, task);
    return queue;
  };

  function flush(): Promise<void> {
    window.clearTimeout(timer);
    timer = undefined;
    firstDirtyAt = 0;

    if (!enabled || dirty.size === 0) return queue;

    const ids = [...dirty];
    dirty.clear();

    return enqueue(async () => {
      for (const id of ids) {
        const dialog = getDialog(id);
        if (!dialog) continue; // deleted meanwhile
        try {
          await io.save(dialog);
        } catch (err) {
          console.error(`Failed to save chat ${id}:`, err);
        }
      }
    });
  }

  return {
    markDirty(id) {
      if (!enabled) return;
      dirty.add(id);

      const now = Date.now();
      if (!firstDirtyAt) firstDirtyAt = now;
      const wait = Math.max(0, Math.min(IDLE_MS, firstDirtyAt + MAX_WAIT_MS - now));

      window.clearTimeout(timer);
      timer = window.setTimeout(() => void flush(), wait);
    },

    flush,

    remove(id) {
      dirty.delete(id);
      if (!enabled) return queue;
      return enqueue(() =>
        io.remove(id).catch((err) => console.error(`Failed to delete chat ${id}:`, err))
      );
    },

    disable() {
      enabled = false;
      dirty.clear();
      window.clearTimeout(timer);
    },
  };
}
