import { batch, createEffect, createSignal, For, Show, type Accessor } from 'solid-js';
import Terminal from './Terminal';

interface TerminalPanelProps {
  collapsed: Accessor<boolean>;
  onCollapsedChange: (value: boolean) => void;
  rootPath: Accessor<string | null>;
}

/**
 * One terminal. Never replaced while it lives: <For> keys by reference, and
 * a new object would remount the view and kill its shell. What changes
 * (shell name, title) lives in `details` instead.
 */
interface TerminalTab {
  readonly key: number;
  /** Folder it was opened in; new terminals follow the current project. */
  readonly cwd: string | null;
}

interface TabDetails {
  /** Shell name once spawned ("bash"). */
  shell?: string;
  /** Window title set by the shell, shown as the tab's tooltip. */
  title?: string;
}

const DEFAULT_HEIGHT = 240;
const MIN_HEIGHT = 120;
const MAX_HEIGHT = 640;

const isMac = navigator.platform.toUpperCase().includes('MAC');

/**
 * Panel shortcuts, handled while focus is in the panel (xterm passes them
 * up instead of sending them to the shell):
 *   Ctrl+Shift+`          new terminal
 *   Ctrl+PageDown / PageUp next / previous terminal
 *   Ctrl+Shift+W           close the current one (Cmd+Shift+W on macOS)
 */
type PanelAction = 'new' | 'next' | 'previous' | 'close';

function panelAction(e: KeyboardEvent): PanelAction | null {
  if (e.type !== 'keydown' || e.altKey) return null;
  const mod = isMac ? e.metaKey : e.ctrlKey;
  if (e.ctrlKey && e.shiftKey && (e.code === 'Backquote' || e.key === '~' || e.key === '`')) {
    return 'new';
  }
  if (e.ctrlKey && !e.shiftKey && e.key === 'PageDown') return 'next';
  if (e.ctrlKey && !e.shiftKey && e.key === 'PageUp') return 'previous';
  if (mod && e.shiftKey && e.key.toLowerCase() === 'w') return 'close';
  return null;
}

export default function TerminalPanel(props: TerminalPanelProps) {
  const [height, setHeight] = createSignal(DEFAULT_HEIGHT);
  const [dragging, setDragging] = createSignal(false);
  const [tabs, setTabs] = createSignal<TerminalTab[]>([]);
  const [activeKey, setActiveKey] = createSignal<number | null>(null);
  const [details, setDetails] = createSignal<Record<number, TabDetails>>({});
  let nextKey = 1;

  const shellOf = (tab: TerminalTab) => details()[tab.key]?.shell ?? 'shell';

  function newTerminal() {
    const key = nextKey++;
    batch(() => {
      setTabs((t) => [...t, { key, cwd: props.rootPath() }]);
      setActiveKey(key);
      if (props.collapsed()) props.onCollapsedChange(false);
    });
  }

  function closeTerminal(key: number) {
    const list = tabs();
    const index = list.findIndex((t) => t.key === key);
    if (index < 0) return;
    const remaining = list.filter((t) => t.key !== key);
    batch(() => {
      // Closing the last one hides the panel; opening it again starts a
      // fresh terminal (the effect below).
      if (remaining.length === 0) props.onCollapsedChange(true);
      if (activeKey() === key) {
        setActiveKey(remaining[Math.min(index, remaining.length - 1)]?.key ?? null);
      }
      setTabs(remaining);
      setDetails(({ [key]: _gone, ...rest }) => rest);
    });
  }

  function cycle(step: number) {
    const list = tabs();
    if (list.length < 2) return;
    const index = list.findIndex((t) => t.key === activeKey());
    setActiveKey(list[(index + step + list.length) % list.length].key);
  }

  function updateDetails(key: number, patch: TabDetails) {
    setDetails((all) => ({ ...all, [key]: { ...all[key], ...patch } }));
  }

  // Opening the panel with no terminal (first open, restored as expanded,
  // or after the last one was closed) starts one.
  createEffect(() => {
    if (!props.collapsed() && tabs().length === 0) newTerminal();
  });

  function runAction(action: PanelAction) {
    if (action === 'new') newTerminal();
    else if (action === 'next') cycle(1);
    else if (action === 'previous') cycle(-1);
    else if (action === 'close') {
      const key = activeKey();
      if (key !== null) closeTerminal(key);
    }
  }

  function handleKeyDown(e: KeyboardEvent) {
    const action = panelAction(e);
    if (!action) return;
    e.preventDefault();
    e.stopPropagation();
    runAction(action);
  }

  /** Tab label: the shell name, numbered when several share it ("bash 2"). */
  function label(tab: TerminalTab) {
    const shell = shellOf(tab);
    const same = tabs().filter((t) => shellOf(t) === shell);
    if (same.length < 2) return shell;
    return `${shell} ${same.findIndex((t) => t.key === tab.key) + 1}`;
  }

  function toggle() {
    props.onCollapsedChange(!props.collapsed());
  }

  function startResize(e: PointerEvent) {
    if (props.collapsed()) return;
    e.preventDefault();
    setDragging(true);
    const startY = e.clientY;
    const startHeight = height();

    function onMove(ev: PointerEvent) {
      const delta = startY - ev.clientY;
      setHeight(Math.min(MAX_HEIGHT, Math.max(MIN_HEIGHT, startHeight + delta)));
    }
    function onUp() {
      setDragging(false);
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
    }
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  }

  return (
    <div
      class="terminal-panel"
      classList={{ collapsed: props.collapsed() }}
      onKeyDown={handleKeyDown}
    >
      <Show when={!props.collapsed()}>
        <div
          class="terminal-resize-handle"
          classList={{ dragging: dragging() }}
          onPointerDown={startResize}
        />
      </Show>

      <div class="terminal-panel-header">
        <button
          class="terminal-toggle-button"
          onClick={toggle}
          title={props.collapsed() ? 'Expand terminal' : 'Collapse terminal'}
          aria-expanded={!props.collapsed()}
        >
          <svg
            class="terminal-toggle-icon"
            classList={{ expanded: !props.collapsed() }}
            width="12"
            height="12"
            viewBox="0 0 16 16"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <path d="M6 4l4 4-4 4" />
          </svg>
          Terminal
          <Show when={props.collapsed() && tabs().length > 1}>
            <span class="terminal-count">{tabs().length}</span>
          </Show>
        </button>

        <Show when={!props.collapsed()}>
          <div class="terminal-tabs" role="tablist" aria-label="Terminals">
            <For each={tabs()}>
              {(tab) => (
                <div
                  class="terminal-tab"
                  classList={{ active: tab.key === activeKey() }}
                  role="tab"
                  tabIndex={tab.key === activeKey() ? 0 : -1}
                  aria-selected={tab.key === activeKey()}
                  title={details()[tab.key]?.title ?? tab.cwd ?? undefined}
                  onClick={() => setActiveKey(tab.key)}
                  onAuxClick={(e) => {
                    // Middle click closes, like editor tabs in most apps.
                    if (e.button === 1) {
                      e.preventDefault();
                      closeTerminal(tab.key);
                    }
                  }}
                >
                  <svg
                    class="terminal-tab-icon"
                    width="12"
                    height="12"
                    viewBox="0 0 16 16"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.5"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                    aria-hidden="true"
                  >
                    <path d="M3 4.5 6.5 8 3 11.5M8 11.5h5" />
                  </svg>
                  <span class="terminal-tab-name">{label(tab)}</span>
                  <button
                    type="button"
                    class="terminal-tab-close"
                    title="Kill terminal"
                    aria-label={`Kill ${label(tab)}`}
                    onClick={(e) => {
                      e.stopPropagation();
                      closeTerminal(tab.key);
                    }}
                  >
                    <svg
                      width="10"
                      height="10"
                      viewBox="0 0 16 16"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="1.8"
                      stroke-linecap="round"
                      aria-hidden="true"
                    >
                      <path d="M4 4l8 8M12 4l-8 8" />
                    </svg>
                  </button>
                </div>
              )}
            </For>
          </div>
        </Show>

        <button
          type="button"
          class="terminal-new-button"
          onClick={newTerminal}
          title={`New terminal (Ctrl+Shift+\`)`}
          aria-label="New terminal"
        >
          <svg
            width="14"
            height="14"
            viewBox="0 0 16 16"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            aria-hidden="true"
          >
            <path d="M8 3.5v9M3.5 8h9" />
          </svg>
        </button>
      </div>

      <div
        class="terminal-panel-body"
        classList={{ 'no-transition': dragging() }}
        style={{ height: props.collapsed() ? '0px' : `${height()}px` }}
      >
        {/* Every terminal stays mounted, so switching keeps each shell and its
            scrollback; only the active one is shown. */}
        <For each={tabs()}>
          {(tab) => (
            <Terminal
              cwd={tab.cwd}
              active={tab.key === activeKey() && !props.collapsed()}
              onSpawned={(shell) => updateDetails(tab.key, { shell })}
              onTitleChange={(title) => updateDetails(tab.key, { title })}
              // A clean exit (`exit`, Ctrl+D) closes the tab. A failing one
              // stays open, so its output and exit code can be read.
              onExit={(code) => code === 0 && closeTerminal(tab.key)}
              isPanelShortcut={(e) => panelAction(e) !== null}
            />
          )}
        </For>
      </div>
    </div>
  );
}
