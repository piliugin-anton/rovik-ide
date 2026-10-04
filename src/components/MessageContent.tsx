import { createEffect, createMemo, createSignal, Index, Match, on, Show, Switch } from 'solid-js';
import { parseMessageSegments } from '../lib/messageSegments';
import { pathBefore } from '../lib/codeLocation';
import { requestOpenFile, resolveProjectFile } from '../lib/editorBridge';
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
      {(segment, index) => {
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

        /*
         * The project file this block is from, for "Open in editor": named in
         * the fence (```rust src/main.rs) or on the line right before it
         * ("In `src/main.rs`:"). Only offered when that file exists.
         */
        const candidate = createMemo(() => {
          const s = segment();
          if (s.type !== 'code') return undefined;
          if (s.file) return s.file;
          const prev = segments()[index - 1];
          return prev?.type === 'text' ? pathBefore(prev.content) : undefined;
        });
        const [file, setFile] = createSignal<string | null>(null);
        createEffect(
          on([candidate, open], ([path, streaming]) => {
            setFile(null);
            if (!path || streaming) return;
            void resolveProjectFile(path).then((abs) => {
              if (candidate() === path) setFile(abs);
            });
          })
        );

        return (
          <Switch>
            <Match when={type() === 'code'}>
              <StaticCodeBlock
                code={text()}
                language={language()}
                streaming={open()}
                // Opens the file where this code is found in it.
                onOpenInEditor={
                  file() ? () => requestOpenFile({ path: file()!, snippet: text() }) : undefined
                }
              />
            </Match>
            <Match when={type() === 'text'}>
              <FormattedText class="message-text" text={text()} />
            </Match>
            <Match when={type() === 'pasted'}>
              <PastedBlock text={text()} lines={(segment() as { lines: number }).lines} />
            </Match>
          </Switch>
        );
      }}
    </Index>
  );
}

/**
 * A long paste in a sent message: collapsed to its size, and only rendered
 * when opened (a few thousand lines in the chat were slow).
 */
function PastedBlock(props: { text: string; lines: number }) {
  const [open, setOpen] = createSignal(false);
  return (
    <div class="chat-pasted-block">
      <button
        type="button"
        class="chat-pasted-toggle"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open()}
      >
        <svg
          class="chat-pasted-caret"
          classList={{ open: open() }}
          width="10"
          height="10"
          viewBox="0 0 16 16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.6"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M6 4l4 4-4 4" />
        </svg>
        Pasted text · {props.lines.toLocaleString()} line{props.lines === 1 ? '' : 's'},{' '}
        {props.text.length.toLocaleString()} characters
      </button>
      <Show when={open()}>
        <pre class="chat-pasted-text-body">{props.text}</pre>
      </Show>
    </div>
  );
}
