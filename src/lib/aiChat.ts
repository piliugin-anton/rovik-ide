// src/lib/aiChat.ts — new file
import { invoke } from "@tauri-apps/api/core";
import type { ChatMessage, ChatStreamEvent, ChatToolCall, ModelProfile } from "../types";
import { Channel } from "@tauri-apps/api/core";

interface ChatCompletionResult {
  content: string;
  toolCalls?: ChatToolCall[];
}

export async function sendChatMessage(
  profile: ModelProfile,
  messages: ChatMessage[],
  channel: Channel<ChatStreamEvent>
): Promise<void> {
  await invoke<string>("send_chat_message", {
    baseUrl: profile.baseUrl,
    apiKey: profile.apiKey,
    model: profile.model,
    messages: messages.map((m) => ({ role: m.role, content: m.content })),
    stream: profile.stream,
    onEvent: channel
  });

  /* const parsed = JSON.parse(raw);
  const choice = parsed.choices?.[0]?.message;
  if (!choice) throw new Error("Unexpected response shape from model API");

  const toolCalls: ChatToolCall[] | undefined = Array.isArray(choice.tool_calls)
    ? choice.tool_calls.map((tc: { id: string; function?: { name?: string; arguments?: string } }) => ({
        id: tc.id,
        name: tc.function?.name ?? "",
        arguments: tc.function?.arguments ?? "",
      }))
    : undefined;

  return { content: choice.content ?? "", toolCalls }; */
}