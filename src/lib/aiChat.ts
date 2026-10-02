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
        messages?: undefined;
      }
    | {
        messages: ChatMessage[];
        dialogId?: undefined;
        currentUserContent?: undefined;
        continueReply?: undefined;
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
