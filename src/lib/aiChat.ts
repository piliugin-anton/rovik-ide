// src/lib/aiChat.ts — new file
import { invoke } from "@tauri-apps/api/core";
import type { ChatMessage, ChatStreamEvent, ModelProfile } from "../types";
import { Channel } from "@tauri-apps/api/core";

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
    streaming: profile.stream,
    onEvent: channel
  });
}