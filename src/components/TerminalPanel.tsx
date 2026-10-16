import { batch, createEffect, createSignal, For, onCleanup, Show, type Accessor } from 'solid-js';
import Terminal from './Terminal';
import TerminalNewMenu from './TerminalNewMenu';
import { cloud } from '../lib/cloud';
import {
  remoteLabel,
  terminalHosts,
  terminalLabel,
  type LabeledTab,
  type TerminalHost,
} from '../lib/terminal';

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
  /**
   * The cloud host it runs on, named as it was when it opened; null on this
   * computer.
   */
  readonly host: TerminalHost | null;
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
  /** Where "+"'s menu is open, if it is. */
  const [menu, setMenu] = createSignal<{ x: number; y: number } | null>(null);
  let nextKey = 1;
  let newButton: HTMLButtonElement | undefined;
  let menuEl: HTMLDivElement | undefined;

  const shellOf = (tab: TerminalTab) => details()[tab.key]?.shell ?? 'shell';

  /** The cloud hosts a terminal can open on now: once there's one, "+" asks where. */
  const remoteHosts = () => terminalHosts(cloud.hosts);
  /** The tabs say where they run once a host is connected, or one of them runs on one. */
  const placed = () => remoteHosts().length > 0 || tabs().some((t) => t.host !== null);
  /** A host's name as the app knows it now, or as it was when the tab opened. */
  const nameOf = (host: TerminalHost) =>
    cloud.hosts.find((h) => h.id === host.id)?.name ?? host.name;

  /** A new terminal, on this computer or on `host`. */
  function newTerminal(host: TerminalHost | null = null) {
    const key = nextKey++;
    batch(() => {
      setMenu(null);
      setTabs((t) => [...t, { key, cwd: host ? null : props.rootPath(), host }]);
      setActiveKey(key);
      if (props.collapsed()) props.onCollapsedChange(false);
    });
  }

  /** "+": a terminal here, or once a host is connected, the menu of where. */
  function onNewClick() {
    if (menu()) {
      setMenu(null);
    } else if (remoteHosts().length === 0) {
      newTerminal();
    } else {
      // Under "+", as wide as a menu is at least, until its size is known
      // (placeMenu).
      const r = newButton!.getBoundingClientRect();
      setMenu({ x: Math.max(0, r.right - 200), y: r.bottom + 4 });
    }
  }

  /**
   * Kept in the window once its size is known: its right edge on "+"'s, and
   * under it, or over it when there's no room below (the panel collapsed).
   */
  function placeMenu(el: HTMLDivElement) {
    menuEl = el;
    queueMicrotask(() => {
      const at = menu();
      if (!at || menuEl !== el || !newButton) return;
      const r = newButton.getBoundingClientRect();
      const below = r.bottom + 4;
      const fits = below + el.offsetHeight <= window.innerHeight - 10;
      const y = fits ? below : Math.max(0, r.top - 4 - el.offsetHeight);
      const right = window.innerWidth - el.offsetWidth - 10;
      const x = Math.max(0, Math.min(r.right - el.offsetWidth, right));
      if (x !== at.x || y !== at.y) setMenu({ x, y });
    });
  }

  // Closed by a press elsewhere, or the window losing focus or size, as the
  // app's other menus are; its own keys close it too (TerminalNewMenu).
  createEffect(() => {
    if (!menu()) return;
    const close = () => setMenu(null);
    const outside = (e: Event) => {
      const target = e.target as Node;
      if (!menuEl?.contains(target) && !newButton?.contains(target)) close();
    };
    window.addEventListener('pointerdown', outside, true);
    window.addEventListener('blur', close);
    window.addEventListener('resize', close);
    onCleanup(() => {
      window.removeEventListener('pointerdown', outside, true);
      window.removeEventListener('blur', close);
      window.removeEventListener('resize', close);
    });
  });

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

  function labeled(tab: TerminalTab): LabeledTab {
    const host = tab.host && { id: tab.host.id, name: nameOf(tab.host) };
    return { key: tab.key, shell: shellOf(tab), host };
  }

  /**
   * Tab label: the shell name, numbered when several share it in one place
   * ("bash 2"), then where it runs once a host is connected: "bash (local)",
   * "bash (Build (remote))".
   */
  function label(tab: TerminalTab) {
    return terminalLabel(labeled(tab), tabs().map(labeled), placed());
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
                  title={
                    details()[tab.key]?.title ??
                    (tab.host ? remoteLabel(nameOf(tab.host)) : tab.cwd) ??
                    undefined
                  }
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
          ref={newButton}
          type="button"
          class="terminal-new-button"
          onClick={onNewClick}
          title={remoteHosts().length > 0 ? 'New terminal' : `New terminal (Ctrl+Shift+\`)`}
          aria-label="New terminal"
          aria-haspopup={remoteHosts().length > 0 ? 'menu' : undefined}
          aria-expanded={remoteHosts().length > 0 ? menu() !== null : undefined}
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
              host={tab.host}
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

      <Show when={menu()}>
        {(at) => (
          <TerminalNewMenu
            ref={placeMenu}
            x={at().x}
            y={at().y}
            hosts={remoteHosts()}
            onPick={newTerminal}
            onClose={() => {
              setMenu(null);
              newButton?.focus();
            }}
          />
        )}
      </Show>
    </div>
  );
}
