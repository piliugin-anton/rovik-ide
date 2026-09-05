export function generateTitle(conversationText: string): string {
  return `You are a chat-title generator for an IDE.

From the conversation below, output ONE short title line (max 6 words) that:
- Describes the main task or topic
- Uses active, concrete wording
- Avoids quotes, special characters, and filler words (a, an, the)
- Is in English and sentence case (first word capitalized only)

Do not add explanations, just the title.

Conversation:
${conversationText}`;
}
