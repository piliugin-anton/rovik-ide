import { For } from 'solid-js';
import { parseMessageSegments } from '../lib/messageSegments';
import CodeBlock from './CodeBlock';

export default function MessageContent(props: { content: string }) {
  const segments = () => parseMessageSegments(props.content);

  return (
    <For each={segments()}>
      {(segment) =>
        segment.type === 'code' ? (
          <CodeBlock code={segment.content} language={segment.language} />
        ) : (
          <span class="message-text">{segment.content}</span>
        )
      }
    </For>
  );
}
