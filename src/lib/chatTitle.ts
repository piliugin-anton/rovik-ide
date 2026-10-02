import type { ChatMessage, ModelProfile } from '../types';
import { completeOnce, LEAN_THEN_PLAIN, stripThinking } from './oneShot';

const MAX_TITLE_CHARS = 60;
const MAX_MESSAGE_CHARS = 1_500;

const SYSTEM_PROMPT = `You name chats in a code editor.

Reply with a title of 2 to 6 words describing the user's main task or topic.
- Concrete wording, sentence case, no quotes, no trailing punctuation
- Use the same language as the user
- Reply with the title only: no explanation, no prefix like "Title:"`;

const ATTEMPTS = LEAN_THEN_PLAIN(64);

/**
 * Generates a short title for a conversation. Falls back to the first words
 * of the user's first message when the model can't produce one, so a chat
 * never stays "New Chat".
 *
 * The title is chat metadata, never added to the stored messages.
 */
export async function generateChatTitle(
  profile: ModelProfile,
  messages: ChatMessage[]
): Promise<string | null> {
  const transcript = messages
    .filter((m) => (m.role === 'user' || m.role === 'assistant') && m.content.trim())
    .map((m) => ({ ...m, content: m.content.slice(0, MAX_MESSAGE_CHARS) }));

  if (!transcript.some((m) => m.role === 'user')) return null;

  // One user message holding the conversation: some providers reject a
  // history that ends with an assistant turn, and it keeps the model from
  // simply continuing the chat.
  const request: ChatMessage[] = [
    {
      id: 'title-request',
      role: 'user',
      content: [
        'Conversation:',
        ...transcript.map((m) => `${m.role === 'user' ? 'User' : 'Assistant'}: ${m.content}`),
        '',
        'Title for this conversation:',
      ].join('\n'),
    },
  ];

  for (const options of ATTEMPTS) {
    const result = await completeOnce({
      profile,
      systemPrompt: SYSTEM_PROMPT,
      messages: request,
      options,
    });
    if (!result.ok) {
      console.warn('Chat title request failed:', result.error);
      continue;
    }
    const title = cleanTitle(result.text);
    if (title) return title;
  }

  return fallbackTitle(transcript.find((m) => m.role === 'user')!.content);
}

/** First usable line, without reasoning blocks, prefixes, quotes or markdown. */
export function cleanTitle(raw: string): string | null {
  const text = stripThinking(raw);

  const line = text
    .split('\n')
    .map((l) => l.trim())
    .find((l) => l.length > 0);
  if (!line) return null;

  const title = line
    .replace(/^#+\s*/, '')
    .replace(/^\*+|\*+$/g, '')
    .replace(/^(chat\s+)?title\s*:\s*/i, '')
    .replace(/^["'`“”‘’«»]+|["'`“”‘’«»]+$/g, '')
    .replace(/[.!:;,\s]+$/, '')
    .replace(/\s+/g, ' ')
    .trim();

  if (!title) return null;
  return truncate(title);
}

/** The opening words of the user's message, used when the model gives nothing. */
export function fallbackTitle(text: string): string | null {
  const words = text.replace(/\s+/g, ' ').trim().split(' ').slice(0, 6).join(' ');
  if (!words) return null;
  return truncate(words.charAt(0).toUpperCase() + words.slice(1));
}

function truncate(title: string): string {
  if (title.length <= MAX_TITLE_CHARS) return title;
  const cut = title.slice(0, MAX_TITLE_CHARS);
  const space = cut.lastIndexOf(' ');
  return `${(space > 20 ? cut.slice(0, space) : cut).trimEnd()}…`;
}
