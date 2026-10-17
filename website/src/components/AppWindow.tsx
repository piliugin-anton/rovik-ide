// A drawing of Rovik's window, after the app's own layout and colors: the
// sidebar's AI Chat tab with a finished agent run, the file it changed in
// the editor, and the tests in the terminal. Shown until a screenshot is
// added (see Showcase.tsx).
import { For } from 'solid-js';
import { highlight } from '../lib/highlight';
import { CheckIcon, ChevronRightIcon, FolderIcon } from './icons';

const CODE = `import { HttpError } from './errors';

export interface RetryOptions {
  retries?: number;
  baseDelayMs?: number;
}

const sleep = (ms: number) => new Promise((done) => setTimeout(done, ms));

export async function fetchJson<T>(url: string, opts: RetryOptions = {}) {
  const { retries = 3, baseDelayMs = 200 } = opts;
  for (let attempt = 0; ; attempt++) {
    const res = await fetch(url);
    if (res.ok) return (await res.json()) as T;
    if (res.status < 500 || attempt === retries) {
      throw new HttpError(res.status, url);
    }
    await sleep(baseDelayMs * 2 ** attempt);
  }
}`;

// The change's lines in the gutter, as the editor marks them.
const ADDED = new Set([3, 4, 5, 6, 8, 11, 12, 15, 16, 17, 18, 19]);
const CHANGED = new Set([10, 13, 14]);

const LINES = CODE.split('\n').map((text, i) => ({
  n: i + 1,
  tokens: highlight(text),
  mark: ADDED.has(i + 1) ? 'added' : CHANGED.has(i + 1) ? 'changed' : '',
}));

// The app's mode switch (ModeSelector.tsx), with Autopilot's double arrow.
const MODES = [
  { label: 'Agent', icon: 'M5 3.5v9l7-4.5-7-4.5Z', fill: true },
  { label: 'Plan', icon: 'M6.5 4.5h7M6.5 8h7M6.5 11.5h7M2.75 4.5h.5M2.75 8h.5M2.75 11.5h.5' },
  { label: 'Autopilot', icon: 'M2.5 4l4.5 4-4.5 4M8.5 4l4.5 4-4.5 4' },
];

// The app's own marks: the pickers' carets (ModelPicker, EffortSelector),
// the project switcher's, the terminal's toggle, and the effort's bulb.
const CARET_UP = 'M4 10l4-4 4 4';
const CARET_DOWN = 'M4 6l4 4 4-4';
const CARET_RIGHT = 'M6 4l4 4-4 4';
const BULB =
  'M8 1.75a4.25 4.25 0 0 0-2.5 7.7V11h5V9.45A4.25 4.25 0 0 0 8 1.75ZM6 13.25h4M6.75 14.75h2.5';

function Glyph(props: { d: string; size?: number; fill?: boolean; class?: string }) {
  return (
    <svg
      class={props.class}
      width={props.size ?? 11}
      height={props.size ?? 11}
      viewBox="0 0 16 16"
      fill={props.fill ? 'currentColor' : 'none'}
      stroke="currentColor"
      stroke-width="1.6"
      stroke-linecap="round"
      stroke-linejoin="round"
    >
      <path d={props.d} />
    </svg>
  );
}

export default function AppWindow() {
  return (
    <div
      class="win"
      role="img"
      aria-label="Rovik IDE: an agent's chat in the sidebar, the TypeScript file it changed in the editor, and its tests passing in the terminal"
    >
      <div class="win-titlebar" aria-hidden="true">
        <span class="win-title">Rovik IDE</span>
        <span class="win-controls">
          <i>–</i>
          <i>□</i>
          <i>×</i>
        </span>
      </div>
      <div class="win-body" aria-hidden="true">
        <div class="win-sidebar">
          <div class="win-project">
            <FolderIcon size={13} />
            <b>acme-api</b>
            <Glyph d={CARET_DOWN} size={10} class="win-dim" />
            <span class="win-gear">⚙</span>
          </div>
          <div class="win-tabs">
            <span>Files</span>
            <span class="active">AI Chat</span>
            <span>Git</span>
          </div>
          <div class="win-chat-header">
            <span class="win-dim">☰</span>
            <span class="win-chat-title">Retry failed requests</span>
            <span class="win-host">● This computer ▾</span>
            <span class="win-plus">+</span>
          </div>
          <div class="win-messages">
            <div class="win-msg win-msg-user">
              Retry fetchJson on 5xx responses with exponential backoff, and add a test.
            </div>
            <div class="win-tools">
              <div>
                <ChevronRightIcon size={11} /> Read 2 files
              </div>
              <div>
                <ChevronRightIcon size={11} /> Edited <code>src/lib/http.ts</code>
                <span class="win-add">+17</span>
                <span class="win-del">−4</span>
              </div>
              <div>
                <ChevronRightIcon size={11} /> Edited <code>src/lib/http.test.ts</code>
                <span class="win-add">+24</span>
              </div>
              <div>
                <ChevronRightIcon size={11} /> Ran <code>npm test -- http</code>
                <span class="win-ok">7 passed</span>
              </div>
            </div>
            <div class="win-checks">
              <CheckIcon size={12} />
              <span>Checks passed</span>
              <span class="win-chip">typecheck</span>
              <span class="win-chip">lint</span>
              <span class="win-chip">test</span>
            </div>
            <div class="win-msg win-msg-agent">
              fetchJson now retries a 5xx up to 3 times, waiting 200, 400 and 800 ms. A 4xx still
              fails at once. The new test covers both.
            </div>
          </div>
          <div class="win-landed">
            <CheckIcon size={12} />
            Landed 2 files
            <span class="win-link">Undo</span>
          </div>
          <div class="win-input">
            <span class="win-dim">Message the model…</span>
            <span class="win-send">
              <svg width="11" height="11" viewBox="0 0 16 16" fill="currentColor">
                <path d="M2 2.5 14.5 8 2 13.5l1.6-4.6L9 8 3.6 7.1Z" />
              </svg>
            </span>
          </div>
          <div class="win-row">
            <span class="win-modes">
              <For each={MODES}>
                {(mode, i) => (
                  <span classList={{ active: i() === 0 }}>
                    <Glyph d={mode.icon} fill={mode.fill} />
                    {mode.label}
                  </span>
                )}
              </For>
            </span>
          </div>
          <div class="win-row">
            <span class="win-select">
              <span class="win-select-label">Ollama/qwen3-coder</span>
              <Glyph d={CARET_UP} size={12} />
            </span>
            <span class="win-select">
              <Glyph d={BULB} size={12} />
              <span class="win-select-label">Default effort</span>
              <Glyph d={CARET_UP} size={10} />
            </span>
          </div>
        </div>
        <div class="win-main">
          <div class="win-editor-tabs">
            <span class="active">http.ts</span>
            <span>http.test.ts</span>
            <span>README.md</span>
          </div>
          <div class="win-code">
            <For each={LINES}>
              {(line) => (
                <div class={`win-line ${line.mark}`}>
                  <span class="win-ln">{line.n}</span>
                  <span class="win-src">
                    <For each={line.tokens}>
                      {(token) =>
                        token.kind ? (
                          <span class={`tk-${token.kind}`}>{token.text}</span>
                        ) : (
                          token.text
                        )
                      }
                    </For>
                  </span>
                </div>
              )}
            </For>
          </div>
          <div class="win-terminal">
            <div class="win-terminal-head">
              <Glyph d={CARET_RIGHT} size={12} class="win-open" />
              Terminal
              <span class="win-term-tab">bash (local)</span>
            </div>
            <div class="win-term">
              <div>
                <span class="win-prompt">~/acme-api $</span> npm test -- http
              </div>
              <div>
                <span class="win-ok"> ✓</span> src/lib/http.test.ts{' '}
                <span class="win-dim">(7 tests) 38ms</span>
              </div>
              <div>
                <span class="win-dim"> Test Files</span> <span class="win-ok">1 passed</span>{' '}
                <span class="win-dim">(1)</span>
              </div>
              <div>
                <span class="win-dim"> Tests</span> <span class="win-ok">7 passed</span>{' '}
                <span class="win-dim">(7)</span>
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
