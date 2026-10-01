// Typed wrappers for the chat database commands (src-tauri/src/chat_db).
import { invoke } from '@tauri-apps/api/core';
import type { ChatMessage, ChatToolCall, DialogCursor, DialogSummary, ToolMeta } from '../types';
import type { Highlighted } from './chatSearch';

interface MessageRow {
  id: string;
  dialogId: string;
  seq: number;
  role: ChatMessage['role'];
  content: string;
  bodyExternal: boolean;
  bodyBytes: number;
  toolCalls: ChatToolCall[] | null;
  toolCallId: string | null;
  meta: ToolMeta | null;
  error: string | null;
  failed: boolean;
  finalized: boolean;
  attachments: string[];
  createdAt: number;
}

export interface MessagePage {
  messages: ChatMessage[];
  hasOlder: boolean;
  hasNewer: boolean;
}

export interface DialogPage {
  items: DialogSummary[];
  nextCursor: DialogCursor | null;
}

export interface ChatSearchHit {
  dialog: DialogSummary;
  score: number;
  messageId: string | null;
  snippet: Highlighted | null;
}

function fromRow(r: MessageRow): ChatMessage {
  return {
    id: r.id,
    role: r.role,
    content: r.content,
    toolCalls: r.toolCalls ?? undefined,
    tool_call_id: r.toolCallId ?? undefined,
    error: r.error ?? undefined,
    failed: r.failed || undefined,
    attachments: r.attachments.length ? r.attachments : undefined,
    seq: r.seq,
    bodyExternal: r.bodyExternal || undefined,
    bodyBytes: r.bodyBytes,
    meta: r.meta ?? undefined,
    finalized: r.finalized,
    createdAt: r.createdAt,
  };
}

async function page(
  promise: Promise<{ messages: MessageRow[]; hasOlder: boolean; hasNewer: boolean }>
) {
  const p = await promise;
  return { messages: p.messages.map(fromRow), hasOlder: p.hasOlder, hasNewer: p.hasNewer };
}

export const chatApi = {
  listDialogs: (cursor: DialogCursor | null, limit = 50) =>
    invoke<DialogPage>('chat_list_dialogs', { cursor, limit }),

  getMessages: (dialogId: string, beforeSeq?: number, limit = 50): Promise<MessagePage> =>
    page(invoke('chat_get_messages', { dialogId, beforeSeq, limit })),

  getMessagesAround: (messageId: string, before = 25, after = 25): Promise<MessagePage> =>
    page(invoke('chat_get_messages_around', { messageId, before, after })),

  getMessageBody: (messageId: string) => invoke<string>('chat_get_message_body', { messageId }),

  createDialog: (dialog: { id: string; title: string; createdAt: number; updatedAt: number }) =>
    invoke<void>('chat_create_dialog', { dialog }),

  appendMessages: (dialogId: string, messages: ChatMessage[]) =>
    invoke<number[]>('chat_append_messages', {
      dialogId,
      messages: messages.map((m) => ({
        id: m.id,
        role: m.role,
        content: m.content,
        toolCalls: m.toolCalls?.length ? m.toolCalls : null,
        toolCallId: m.tool_call_id ?? null,
        meta: m.meta ?? null,
        error: m.error ?? null,
        failed: !!m.failed,
        attachments: m.attachments ?? [],
        createdAt: m.createdAt ?? Date.now(),
        finalized: !!m.finalized,
      })),
    }),

  updateMessage: (
    messageId: string,
    patch: {
      content?: string;
      toolCalls?: ChatToolCall[];
      meta?: ToolMeta;
      error?: string;
      failed?: boolean;
      finalize?: boolean;
    }
  ) => invoke<void>('chat_update_message', { messageId, patch }),

  renameDialog: (dialogId: string, title: string) =>
    invoke<void>('chat_rename_dialog', { dialogId, title }),

  deleteDialog: (dialogId: string) => invoke<void>('chat_delete_dialog', { dialogId }),

  search: (query: string, limit = 50) => invoke<ChatSearchHit[]>('chat_search', { query, limit }),

  messagesAnswered: (messageIds: string[]) =>
    invoke<string[]>('chat_messages_answered', { messageIds }),
};

/**
 * URL for an attachment served by the `chat-attachment` scheme. WebView2
 * (Windows) only reaches custom schemes through http://<scheme>.localhost.
 */
export function attachmentUrl(id: string): string {
  const windows = typeof navigator !== 'undefined' && /Windows/i.test(navigator.userAgent);
  return windows ? `http://chat-attachment.localhost/${id}` : `chat-attachment://localhost/${id}`;
}
