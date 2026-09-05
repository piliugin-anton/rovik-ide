import { For } from 'solid-js';
import { parseMessageSegments } from '../lib/messageSegments';
import CodeBlock from './CodeBlock';
import FormattedText from './FormattedText';

export default function MessageContent(props: { content: string }) {
  const segments = () => parseMessageSegments(props.content);

  return (
    <For each={segments()}>
      {(segment) =>
        segment.type === 'code' ? (
          <CodeBlock code={segment.content} language={segment.language} />
        ) : (
          <FormattedText class="message-text" text={segment.content} />
        )
      }
    </For>
  );
}
