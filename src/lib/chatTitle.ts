import { Channel } from '@tauri-apps/api/core';
import type { ChatMessage, ChatStreamEvent, ModelProfile } from '../types';
import { sendChatMessage, streamEndSignal, untilStreamEnd } from './aiChat';

/**
 * Generates a short title for a conversation.
 *
 * The generated title is deliberately not added to the conversation's
 * persisted messages. It is metadata belonging to the chat.
 */
export async function generateChatTitle(
  profile: ModelProfile,
  messages: ChatMessage[]
): Promise<string | null> {
  const channel = new Channel<ChatStreamEvent>();

  let result = '';
  let failed = false;
  const stream = streamEndSignal();

  channel.onmessage = (event) => {
    switch (event.type) {
      case 'Delta':
        result += event.content;
        break;

      case 'Error':
        failed = true;
        console.error('Chat title generation failed:', event.error.message);
        stream.end();
        break;

      case 'Done':
        stream.end();
        break;

      case 'ToolCallStart':
      case 'ToolCallArgumentsDelta':
      case 'ToolCallComplete':
        // Title generation should never request tools.
        break;
    }
  };

  try {
    await sendChatMessage({
      profile,
      channel,
      messages,
      options: {
        max_tokens: 20,
        temperature: 0.01,
        top_p: 1,
        reasoning_effort: 'none',
      },
      systemPrompt: `You are a chat-title generator for an IDE.

From the conversation below, output ONE short title line (max 6 words) that:
- Describes the main task or topic
- Uses active, concrete wording
- Avoids quotes, special characters, and filler words (a, an, the)
- Is in English and sentence case (first word capitalized only)

If the conversation is too short, ambiguous, or you cannot infer a clear topic, output an empty string.

Do not add explanations, notes, or any text other than the title (or an empty string).`,
    });
    // The last deltas can arrive after the command returns.
    await untilStreamEnd(stream.ended);
  } catch (err) {
    console.error('Failed to generate chat title:', err);
    return null;
  }

  if (failed) {
    return null;
  }

  const title = result
    .replace(/^["'`]+|["'`]+$/g, '')
    .replace(/\s+/g, ' ')
    .trim();

  if (!title) {
    return null;
  }

  return title.slice(0, 100);
}
