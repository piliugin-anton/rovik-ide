import { invoke } from '@tauri-apps/api/core';
import type { ChatMessage, ChatStreamEvent, ModelProfile, OpenAIChatOptions } from '../types';
import { Channel } from '@tauri-apps/api/core';

interface SendChatMessageArgs {
  profile: ModelProfile;
  messages: ChatMessage[];
  channel: Channel<ChatStreamEvent>;
  systemPrompt?: string;
  tools?: unknown[];
  options?: OpenAIChatOptions;
}

/** Older screenshots are replaced by a note: each image costs vision tokens on every turn. */
const MAX_IMAGES_IN_HISTORY = 2;

type WireContent = string | { type: string; text?: string; image_url?: { url: string } }[];

interface WireMessage {
  role: ChatMessage['role'];
  content: WireContent;
  tool_calls?: { id: string; type: 'function'; function: { name: string; arguments: string } }[];
  tool_call_id?: string;
}

/**
 * Chat Completions only allows text in role:"tool" messages, so screenshots
 * travel in a user message right after the tool results. It must come after
 * the *whole* run of tool messages: all results for one assistant turn have
 * to directly follow it, or the API rejects the request.
 */
export function toWireMessages(messages: ChatMessage[], vision: boolean): WireMessage[] {
  const imageIndexes = messages.flatMap((m, i) => (m.images?.length ? [i] : []));
  const attach = new Set(vision ? imageIndexes.slice(-MAX_IMAGES_IN_HISTORY) : []);

  const out: WireMessage[] = [];
  let pendingImages: { toolCallId?: string; urls: string[] }[] = [];

  const flushImages = () => {
    for (const entry of pendingImages) {
      out.push({
        role: 'user',
        content: [
          { type: 'text', text: `Screenshot from tool call ${entry.toolCallId ?? ''}:`.trim() },
          ...entry.urls.map((url) => ({ type: 'image_url', image_url: { url } })),
        ],
      });
    }
    pendingImages = [];
  };

  messages.forEach((m, i) => {
    if (m.role !== 'tool') flushImages();

    let content = m.content ?? '';
    if (m.images?.length) {
      if (attach.has(i)) {
        pendingImages.push({ toolCallId: m.tool_call_id, urls: m.images });
      } else {
        content = `${content}\n[The screenshot is no longer attached.]`;
      }
    }

    out.push({
      role: m.role,
      content,
      // Only present on an assistant message that requested tool calls.
      tool_calls: m.toolCalls?.length
        ? m.toolCalls.map((tc) => ({
            id: tc.id,
            type: 'function' as const,
            function: { name: tc.name, arguments: tc.arguments },
          }))
        : undefined,
      // Only present on a role:"tool" reply to one of the above.
      tool_call_id: m.tool_call_id,
    });
  });

  flushImages();
  return out;
}

export async function sendChatMessage(args: SendChatMessageArgs): Promise<void> {
  const { profile, systemPrompt, messages, channel, tools, options } = args;

  await invoke<string>('send_chat_message', {
    baseUrl: profile.baseUrl,
    apiKey: profile.apiKey,
    model: profile.model,
    systemPrompt,
    messages: toWireMessages(messages, profile.vision),
    streaming: profile.stream,
    tools,
    options,
    onEvent: channel,
  });
}
