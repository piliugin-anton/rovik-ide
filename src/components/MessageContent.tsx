import { createMemo, Index, Match, Switch } from 'solid-js';
import { parseMessageSegments } from '../lib/messageSegments';
import StaticCodeBlock from './StaticCodeBlock';
import FormattedText from './FormattedText';

/*
 * While a reply streams, `content` changes every frame and the parser
 * returns brand-new segment objects each time. <For> tracks items by object
 * identity, so it used to rebuild every segment on every frame, including
 * re-creating each Monaco editor and re-parsing all markdown.
 *
 * <Index> tracks by position instead. Earlier segments are finished and
 * never change, so they keep their components and DOM; only the last,
 * growing segment updates. The per-field memos stop the new-but-equal
 * segment objects from triggering work downstream.
 */
export default function MessageContent(props: { content: string; streaming?: boolean }) {
  const segments = createMemo(() => parseMessageSegments(props.content));

  return (
    <Index each={segments()}>
      {(segment) => {
        const type = createMemo(() => segment().type);
        const text = createMemo(() => segment().content);
        const language = createMemo(() => {
          const s = segment();
          return s.type === 'code' ? s.language : undefined;
        });
        // Only the unterminated fence of a streaming reply is incomplete.
        const open = createMemo(() => {
          const s = segment();
          return s.type === 'code' && !!s.open && !!props.streaming;
        });

        return (
          <Switch>
            <Match when={type() === 'code'}>
              <StaticCodeBlock code={text()} language={language()} streaming={open()} />
            </Match>
            <Match when={type() === 'text'}>
              <FormattedText class="message-text" text={text()} />
            </Match>
          </Switch>
        );
      }}
    </Index>
  );
}
