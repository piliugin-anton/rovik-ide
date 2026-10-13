// Typed wrappers for the chat database commands (src-tauri/src/chat_db).
import { invoke } from '@tauri-apps/api/core';
import type { ChatMessage, ChatToolCall, DialogCursor, DialogSummary, ToolMeta } from '../types';
import type { Highlighted } from './chatSearch';
import type { Effort } from './reasoning';

/** A message as the database returns it (chat_db/store.rs). */
export interface MessageRow {
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

export function fromRow(r: MessageRow): ChatMessage {
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
  /** A project's chats (`projectId` null: the chats with no project). */
  listDialogs: (projectId: string | null, cursor: DialogCursor | null, limit = 50) =>
    invoke<DialogPage>('chat_list_dialogs', { projectId, cursor, limit }),
  /**
   * The project a folder is: by the ID file in it (.rovik/project.json),
   * so renaming or moving the folder keeps its chats; else by its path;
   * else a new one.
   */
  openProject: (root: string) => invoke<OpenedProject>('chat_open_project', { root }),
  /** "Looks like a copy": share the original's chats, or a separate history. */
  answerCopy: (root: string, originalId: string, share: boolean) =>
    invoke<ProjectInfo>('chat_answer_copy', { root, originalId, share }),
  /** "Looks like a moved project": use its chats here. */
  answerMoved: (root: string, currentId: string, targetId: string) =>
    invoke<ProjectInfo>('chat_answer_moved', { root, currentId, targetId }),
  /** Not suggested for this folder again. */
  declineProjects: (root: string, projectIds: string[]) =>
    invoke<void>('chat_decline_projects', { root, projectIds }),
  listProjects: () => invoke<ProjectSummary[]>('chat_list_projects'),
  relinkProject: (projectId: string, root: string, merge: boolean) =>
    invoke<Relinked>('chat_relink_project', { projectId, root, merge }),
  deleteProject: (projectId: string) => invoke<void>('chat_delete_project', { projectId }),

  getMessages: (dialogId: string, beforeSeq?: number, limit = 50): Promise<MessagePage> =>
    page(invoke('chat_get_messages', { dialogId, beforeSeq, limit })),

  getMessagesAround: (messageId: string, before = 25, after = 25): Promise<MessagePage> =>
    page(invoke('chat_get_messages_around', { messageId, before, after })),

  getMessageBody: (messageId: string) => invoke<string>('chat_get_message_body', { messageId }),

  createDialog: (dialog: {
    id: string;
    title: string;
    createdAt: number;
    updatedAt: number;
    projectId: string | null;
  }) => invoke<void>('chat_create_dialog', { dialog }),

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

  /** Searches one project's chats (`projectId` null: the chats with no project). */
  search: (query: string, projectId: string | null, limit = 50) =>
    invoke<ChatSearchHit[]>('chat_search', { query, projectId, limit }),

  messagesAnswered: (messageIds: string[]) =>
    invoke<string[]>('chat_messages_answered', { messageIds }),

  /**
   * Debounced save of the chat input's unsent input (one row per chat,
   * `composer_drafts` table). `content` is the JSON-encoded `DraftSegment[]`
   * (lib/composerDraft.ts); empty text deletes the row.
   */
  saveComposerDraft: (dialogId: string, content: string) =>
    invoke<void>('chat_save_composer_draft', { dialogId, content }),

  /** The chat's unsent input (JSON-encoded segments, null when none saved). */
  getComposerDraft: (dialogId: string) =>
    invoke<string | null>('chat_get_composer_draft', { dialogId }),

  /** Saves the model and effort a chat uses, so reopening it continues with them. */
  setModel: (dialogId: string, model: ChatModel) =>
    invoke<void>('chat_set_model', { dialogId, model }),

  /** The chat's saved model and effort (null: never saved). */
  getModel: (dialogId: string) => invoke<ChatModel | null>('chat_get_model', { dialogId }),
};

/** The model a chat last used (`dialogs.model_provider`/`model_id`/`effort`). */
export interface ChatModel {
  providerId: string;
  modelId: string;
  effort: Effort | null;
}

/**
 * URL for an attachment served by the `chat-attachment` scheme. WebView2
 * (Windows) only reaches custom schemes through http://<scheme>.localhost.
 */
export function attachmentUrl(id: string): string {
  const windows = typeof navigator !== 'undefined' && /Windows/i.test(navigator.userAgent);
  return windows ? `http://chat-attachment.localhost/${id}` : `chat-attachment://localhost/${id}`;
}

/* Projects (src-tauri/src/chat_db/projects.rs) */

export interface ProjectInfo {
  id: string;
  name: string;
  path: string;
  /** The ID file is in the folder (else it's known by its path only). */
  idFile: boolean;
}

export interface ProjectSummary {
  id: string;
  name: string;
  path: string;
  chatCount: number;
  lastOpenedAt: number;
  /** None of its folders exist (or have its ID) anymore. */
  missing: boolean;
}

export type ProjectSuggestion =
  { kind: 'copy'; original: ProjectSummary } | { kind: 'moved'; candidates: ProjectSummary[] };

export interface OpenedProject {
  project: ProjectInfo;
  suggestion: ProjectSuggestion | null;
}

export type Relinked =
  { kind: 'done'; project: ProjectInfo } | { kind: 'isOtherProject'; other: ProjectSummary };
