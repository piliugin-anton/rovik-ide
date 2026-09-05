// src/lib/aiChat.ts — new file
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

export async function sendChatMessage(args: SendChatMessageArgs): Promise<void> {
  const { profile, systemPrompt, messages, channel, tools, options } = args;

  await invoke<string>('send_chat_message', {
    baseUrl: profile.baseUrl,
    apiKey: profile.apiKey,
    model: profile.model,
    systemPrompt,
    messages: messages.map((m) => ({
      role: m.role,
      content: m.content ?? '',
      // Only present on an assistant message that requested tool calls.
      tool_calls: m.toolCalls?.length
        ? m.toolCalls.map((tc) => ({
            id: tc.id,
            type: 'function',
            function: { name: tc.name, arguments: tc.arguments },
          }))
        : undefined,
      // Only present on a role:"tool" reply to one of the above.
      tool_call_id: m.tool_call_id,
    })),
    streaming: profile.stream,
    tools,
    options,
    onEvent: channel,
  });
}
