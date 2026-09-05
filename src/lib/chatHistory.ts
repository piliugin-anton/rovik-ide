import { invoke } from '@tauri-apps/api/core';
import type { ChatDialog } from '../types';

interface PersistedChatHistory {
  dialogs: ChatDialog[];
}

const EMPTY: PersistedChatHistory = {
  dialogs: [],
};

export async function loadChatHistory(): Promise<ChatDialog[]> {
  try {
    const raw = await invoke<string>('load_chat_history');
    const parsed = JSON.parse(raw) as Partial<PersistedChatHistory>;

    if (!Array.isArray(parsed.dialogs)) {
      return [];
    }

    return parsed.dialogs
      .filter(
        (dialog): dialog is ChatDialog =>
          !!dialog &&
          typeof dialog === 'object' &&
          typeof dialog.id === 'string' &&
          typeof dialog.title === 'string' &&
          Array.isArray(dialog.messages)
      )
      .sort((a, b) => b.updatedAt - a.updatedAt);
  } catch (err) {
    console.error('Failed to load chat history:', err);
    return EMPTY.dialogs;
  }
}

export async function saveChatHistory(dialogs: ChatDialog[]): Promise<void> {
  await invoke<void>('save_chat_history', {
    json: JSON.stringify({
      dialogs,
    }),
  });
}
