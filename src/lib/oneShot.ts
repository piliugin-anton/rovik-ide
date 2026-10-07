// oneShot.ts
//
// A single model request outside any chat (titles, approval reviews): the
// messages are given explicitly and nothing is stored.
import { Channel } from '@tauri-apps/api/core';
import type {
  ChatError,
  ChatMessage,
  ChatStreamEvent,
  ModelProfile,
  OpenAIChatOptions,
} from '../types';
import { sendChatMessage, streamEndSignal, untilStreamEnd } from './aiChat';
import { noteApiRoute } from './agentRuns';

export type OneShotResult = { ok: true; text: string } | { ok: false; error: string };

export async function completeOnce(args: {
  profile: ModelProfile;
  systemPrompt: string;
  messages: ChatMessage[];
  options?: OpenAIChatOptions;
}): Promise<OneShotResult> {
  const channel = new Channel<ChatStreamEvent>();
  let text = '';
  let failure: ChatError | null = null;
  const stream = streamEndSignal();

  channel.onmessage = (event) => {
    switch (event.type) {
      case 'Delta':
        text += event.content;
        break;
      case 'Error':
        failure = event.error;
        stream.end();
        break;
      case 'Done':
        stream.end();
        break;
      case 'ApiRoute':
        noteApiRoute(args.profile.id, event.route);
        break;
      default:
        // No tools are offered, so tool-call events are ignored.
        break;
    }
  };

  try {
    await sendChatMessage({
      profile: args.profile,
      channel,
      messages: args.messages,
      options: args.options,
      systemPrompt: args.systemPrompt,
    });
    // The last deltas can arrive after the command returns.
    await untilStreamEnd(stream.ended);
  } catch (err) {
    return { ok: false, error: String(err) };
  }

  const error = failure as ChatError | null;
  if (error) return { ok: false, error: `${error.kind}: ${error.message}` };
  return { ok: true, text };
}

/*
 * Providers disagree on request options: OpenAI rejects unknown fields such
 * as `chat_template_kwargs` and, for reasoning models, `max_tokens`; other
 * models ignore `reasoning_effort` and think until the token limit runs out.
 * So callers try a cheap request first and a plain one (no options) after.
 */
export const LEAN_THEN_PLAIN = (maxTokens: number): (OpenAIChatOptions | undefined)[] => [
  { max_tokens: maxTokens, temperature: 0, reasoning_effort: 'none' },
  undefined,
];

/** Removes reasoning blocks some local models put into the content. */
export function stripThinking(raw: string): string {
  return raw.replace(/<think(?:ing)?>[\s\S]*?(<\/think(?:ing)?>|$)/gi, '').trim();
}
