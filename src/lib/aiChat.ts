import { invoke, Channel } from '@tauri-apps/api/core';
import type { ChatMessage, ChatStreamEvent, ModelProfile, OpenAIChatOptions } from '../types';
import { historyBudget } from './contextBudget';

interface CommonArgs {
  profile: ModelProfile;
  channel: Channel<ChatStreamEvent>;
  systemPrompt?: string;
  tools?: unknown[];
  options?: OpenAIChatOptions;
  /** Lets cancelChatRequest stop this request and its retries. */
  requestId?: string;
}

/**
 * Either a chat by id (Rust loads its history from the database and fits it
 * to the model's context window), or explicit messages for one-off requests
 * such as title generation.
 */
type SendChatMessageArgs = CommonArgs &
  (
    | {
        dialogId: string;
        currentUserContent?: string;
        /** Ask the model to continue its interrupted last reply. */
        continueReply?: boolean;
        /**
         * The stored pending assistant message this reply fills. The backend
         * saves provider data for its tool calls (Gemini thought signatures)
         * against it, without relying on the frontend to relay it back.
         */
        assistantMessageId?: string;
        messages?: undefined;
      }
    | {
        messages: ChatMessage[];
        dialogId?: undefined;
        currentUserContent?: undefined;
        continueReply?: undefined;
        assistantMessageId?: undefined;
      }
  );

export async function sendChatMessage(args: SendChatMessageArgs): Promise<void> {
  const { profile, systemPrompt, channel, tools, options } = args;

  await invoke<string>('send_chat_message', {
    baseUrl: profile.baseUrl,
    apiKey: profile.apiKey,
    model: profile.model,
    systemPrompt,
    messages: args.messages?.map((m) => ({ role: m.role, content: m.content ?? '' })),
    dialogId: args.dialogId,
    tokenBudget: historyBudget(profile.contextTokens),
    vision: profile.vision,
    currentUserContent: args.currentUserContent,
    continueReply: args.continueReply,
    assistantMessageId: args.assistantMessageId,
    streaming: profile.stream,
    requestId: args.requestId,
    firstByteTimeoutMs: profile.firstByteTimeoutMs,
    tools,
    options,
    onEvent: channel,
  });
}

/** Stops a running request (and any pending retry). */
export function cancelChatRequest(requestId: string): Promise<boolean> {
  return invoke<boolean>('cancel_chat_request', { requestId });
}

/*
 * Tauri delivers Channel events separately from a command's result, and
 * events of 8 KB or more take an extra asynchronous fetch. So the last
 * events of a stream (a tool call carrying a Gemini thought signature, a
 * final Error) can arrive *after* `sendChatMessage` resolves. Callers wait
 * for the stream's own end, Done or Error, before using what it produced.
 * Events stay in order, so everything before Done/Error has arrived by then.
 */

/** Resolves when the stream reports Done or Error; call `end()` for both. */
export function streamEndSignal(): { ended: Promise<void>; end: () => void } {
  let end!: () => void;
  const ended = new Promise<void>((resolve) => {
    end = resolve;
  });
  return { ended, end };
}

/** Safety net if the final event never arrives. */
export const STREAM_END_GRACE_MS = 10_000;

/** Waits for the stream's end, at most `graceMs` after the command returned. */
export async function untilStreamEnd(
  ended: Promise<void>,
  graceMs = STREAM_END_GRACE_MS
): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timedOut = await Promise.race([
    ended.then(() => false),
    new Promise<boolean>((resolve) => {
      timer = setTimeout(() => resolve(true), graceMs);
    }),
  ]);
  clearTimeout(timer);
  if (timedOut) console.warn('Chat stream: no Done/Error event arrived; continuing without it.');
}
