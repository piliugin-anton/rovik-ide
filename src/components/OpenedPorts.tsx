// OpenedPorts.tsx
//
// What a chat has running, in a row of its own under the chat's title row:
// the commands its agent left running (bash's `background`: a dev server, a
// watcher; lib/backgroundTasks.ts), and the ports a remote chat's servers
// opened on its team server that a page was opened from (D20). "Opened
// ports: 3000 5173" with ports alone; "Background tasks: 2, open ports:
// 3000" with tasks, which opens into the tasks, how long each has run, and
// a Stop for each. A port is the server's own, on a button that does
// what's chosen with its ▾, for every port: open the server's page in a
// browser tab of the editor (the default; lib/browserTabs.ts), or copy the
// address on this computer that leads to it, localhost and the port
// forwarded here, which differs where the server's port was taken on this
// computer. A port whose pages no tab can show (an https server's) copies.
// The choice is remembered. Hidden while the chat has neither (lib/cloud.ts
// keeps the ports).
import { createEffect, createSignal, For, on, onCleanup, onMount, Show } from 'solid-js';
import {
  backgroundTasksOf,
  loadBackgroundTasks,
  stopBackgroundTask,
  type BackgroundTask,
} from '../lib/backgroundTasks';
import { openPage } from '../lib/browserTabs';
import { formatDuration } from '../lib/chatErrors';
import { cloud, onCaughtUp, type ForwardedPort } from '../lib/cloud';
import { focusMenuItem, onMenuKey } from '../lib/menuKeys';
import { GlobeIcon } from './BrowserView';

/** The address on this computer that leads to the server's port. */
export const localAddress = (port: ForwardedPort) => `localhost:${port.localPort}`;

/** What a port's button does: open the server's page in a tab, or copy its address. */
export type PortAction = 'open' | 'copy';

const ACTION_KEY = 'rovik.openedPorts.action';

function storedAction(): PortAction {
  try {
    return localStorage.getItem(ACTION_KEY) === 'copy' ? 'copy' : 'open';
  } catch {
    return 'open';
  }
}

/** What the ports' buttons do, as last chosen: Open until then. */
const [chosen, setChosen] = createSignal<PortAction>(storedAction());

function choose(action: PortAction) {
  setChosen(action);
  try {
    localStorage.setItem(ACTION_KEY, action);
  } catch {
    // not remembered
  }
}

export default function OpenedPorts(props: { dialogId: string | null }) {
  const ports = () => (props.dialogId ? (cloud.ports[props.dialogId] ?? []) : []);
  const tasks = () => backgroundTasksOf(props.dialogId);
  const [open, setOpen] = createSignal(false);
  // What the chat has running, asked where it runs as it opens; closed.
  createEffect(
    on(
      () => props.dialogId,
      (dialogId) => {
        setOpen(false);
        if (dialogId) void loadBackgroundTasks(dialogId);
      }
    )
  );
  // And again as a host catches up: the chat's may have been away.
  onCleanup(
    onCaughtUp(() => {
      if (props.dialogId) void loadBackgroundTasks(props.dialogId);
    })
  );
  // How long each task has run, while they show.
  const [now, setNow] = createSignal(Date.now());
  createEffect(() => {
    if (!open()) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    onCleanup(() => window.clearInterval(timer));
  });
  return (
    <Show when={ports().length > 0 || tasks().length > 0}>
      <div class="opened-ports">
        <div class="opened-ports-row">
          <Show
            when={tasks().length > 0}
            fallback={<span class="opened-ports-label">Opened ports:</span>}
          >
            <button
              type="button"
              class="background-tasks-toggle"
              aria-expanded={open()}
              title={open() ? 'Hide the background tasks' : 'Show the background tasks'}
              onClick={() => setOpen(!open())}
            >
              <span class="background-tasks-caret" aria-hidden="true">
                {open() ? '▾' : '▸'}
              </span>
              <span class="opened-ports-label">
                Background tasks: {tasks().length}
                {ports().length > 0 ? ', open ports:' : ''}
              </span>
            </button>
          </Show>
          <For each={ports()}>{(port) => <OpenedPort port={port} />}</For>
        </div>
        <Show when={open() && tasks().length > 0 && props.dialogId}>
          {(dialogId) => (
            <ul class="background-tasks" aria-label="Background tasks">
              <For each={tasks()}>
                {(task) => <BackgroundTaskRow dialogId={dialogId()} task={task} now={now()} />}
              </For>
            </ul>
          )}
        </Show>
      </div>
    </Show>
  );
}

/** A task: its command, how long it has run, and its Stop, far right. */
function BackgroundTaskRow(props: { dialogId: string; task: BackgroundTask; now: number }) {
  const [stopping, setStopping] = createSignal(false);
  const [failed, setFailed] = createSignal<string | null>(null);
  const ran = () => formatDuration(props.now - props.task.startedAt);

  async function stop() {
    setStopping(true);
    setFailed(null);
    try {
      await stopBackgroundTask(props.dialogId, props.task.id);
    } catch (err) {
      setFailed(`It didn't stop: ${err}`);
    } finally {
      setStopping(false);
    }
  }

  return (
    <li class="background-task">
      <span class="background-task-command" title={props.task.command}>
        {props.task.command}
      </span>
      <Show when={failed()}>
        {(why) => (
          <span class="background-task-failed" role="alert">
            {why()}
          </span>
        )}
      </Show>
      <span class="background-task-ran" title={`Running for ${ran()}`}>
        {ran()}
      </span>
      <button
        type="button"
        class="background-task-stop"
        disabled={stopping()}
        title="Stop it, with everything it started"
        aria-label={`Stop ${props.task.command}`}
        onClick={() => void stop()}
      >
        {stopping() ? 'Stopping…' : 'Stop'}
      </button>
    </li>
  );
}

function OpenedPort(props: { port: ForwardedPort }) {
  const [copied, setCopied] = createSignal(false);
  const [menu, setMenu] = createSignal<{ x: number; y: number } | null>(null);
  let pick: HTMLButtonElement | undefined;
  let menuEl: HTMLDivElement | undefined;
  const address = () => localAddress(props.port);
  /** A browser tab can show the server's pages: an http server's. */
  const inTab = () => props.port.tabPort !== undefined;
  /** What the button does now: what's chosen, or a copy where no tab can open. */
  const action = (): PortAction => (chosen() === 'open' && inTab() ? 'open' : 'copy');
  const label = () => {
    if (action() === 'open') return `Open ${address()} in a tab`;
    return copied() ? 'Copied' : `Copy ${address()}`;
  };
  const where = () =>
    props.port.localPort === props.port.port
      ? `The chat's server listens on port ${props.port.port} on the server: ${address()} on this computer leads to it.`
      : `The chat's server listens on port ${props.port.port} on the server: ${address()} on this computer leads to it, as its own port was taken here.`;

  async function run(what: PortAction) {
    if (what === 'open') return openPage(`http://${address()}/`);
    try {
      await navigator.clipboard.writeText(address());
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1200);
    } catch (err) {
      console.error('Copy failed:', err);
    }
  }

  const close = () => setMenu(null);

  /** Picked in the menu: done, and what the ports' buttons do from now on. */
  function picked(what: PortAction) {
    close();
    choose(what);
    void run(what);
  }

  function toggle() {
    if (menu()) return close();
    const r = pick!.getBoundingClientRect();
    setMenu({ x: r.left, y: r.bottom + 4 });
  }

  /** Kept inside the window, once its size is known. */
  function placeMenu(el: HTMLDivElement) {
    menuEl = el;
    requestAnimationFrame(() => {
      const at = menu();
      if (!at || menuEl !== el) return;
      const x = Math.max(0, Math.min(at.x, window.innerWidth - el.offsetWidth - 10));
      if (x !== at.x) setMenu({ ...at, x });
    });
  }

  // Closed by a press elsewhere, or the window losing focus or size, as the
  // app's other menus are; its own keys close it too (PortMenu).
  createEffect(() => {
    if (!menu()) return;
    const outside = (e: Event) => {
      const target = e.target as Node;
      if (!menuEl?.contains(target) && !pick?.contains(target)) close();
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

  return (
    <span class="opened-port">
      <button
        type="button"
        class="opened-port-main"
        classList={{ copied: copied() }}
        onClick={() => void run(action())}
        title={`${label()}. ${where()}`}
        aria-label={label()}
      >
        <span class="opened-port-number">{props.port.port}</span>
        <Show
          when={action() === 'open'}
          fallback={
            <Show when={copied()} fallback={<CopyIcon />}>
              <CheckIcon />
            </Show>
          }
        >
          <GlobeIcon />
        </Show>
      </button>
      <button
        ref={pick}
        type="button"
        class="opened-port-pick"
        aria-haspopup="menu"
        aria-expanded={!!menu()}
        title="Choose what the ports' buttons do"
        aria-label={`Choose what port ${props.port.port}'s button does`}
        onClick={toggle}
      >
        ▾
      </button>
      <Show when={menu()}>
        {(at) => (
          <PortMenu
            ref={placeMenu}
            x={at().x}
            y={at().y}
            port={props.port.port}
            address={address()}
            inTab={inTab()}
            action={action()}
            onPick={picked}
            onClose={() => {
              close();
              pick?.focus();
            }}
          />
        )}
      </Show>
    </span>
  );
}

/**
 * The ▾ menu of a port's button: Open in a tab, or Copy its address, the
 * one its button does now checked and in focus as it opens. It takes the
 * keyboard as the app's other small menus do (lib/menuKeys.ts).
 */
function PortMenu(props: {
  ref: (el: HTMLDivElement) => void;
  x: number;
  y: number;
  port: number;
  address: string;
  inTab: boolean;
  action: PortAction;
  onPick: (action: PortAction) => void;
  onClose: () => void;
}) {
  let list: HTMLUListElement | undefined;
  onMount(() => focusMenuItem(list, props.action === 'open' ? 0 : 1));

  return (
    <div
      ref={props.ref}
      class="context-menu opened-port-menu"
      style={{ top: `${props.y}px`, left: `${props.x}px` }}
      onContextMenu={(e) => e.preventDefault()}
      onKeyDown={(e) => onMenuKey(e, list, props.onClose)}
    >
      <ul ref={list} class="context-menu-list" role="menu" aria-label={`Port ${props.port}`}>
        <li
          class="context-menu-item"
          classList={{ disabled: !props.inTab }}
          role="menuitemradio"
          aria-checked={props.action === 'open'}
          aria-disabled={!props.inTab}
          tabIndex={-1}
          title={props.inTab ? undefined : 'A browser tab can’t show an https server’s pages'}
          onClick={(e) => {
            e.stopPropagation();
            if (props.inTab) props.onPick('open');
          }}
        >
          <span class="context-menu-icon" aria-hidden="true">
            <GlobeIcon />
          </span>
          <span class="context-menu-label">Open in a tab</span>
          <span class="context-menu-shortcut" aria-hidden="true">
            {props.action === 'open' ? '✓' : ''}
          </span>
        </li>
        <li
          class="context-menu-item"
          role="menuitemradio"
          aria-checked={props.action === 'copy'}
          tabIndex={-1}
          onClick={(e) => {
            e.stopPropagation();
            props.onPick('copy');
          }}
        >
          <span class="context-menu-icon" aria-hidden="true">
            <CopyIcon />
          </span>
          <span class="context-menu-label">Copy {props.address}</span>
          <span class="context-menu-shortcut" aria-hidden="true">
            {props.action === 'copy' ? '✓' : ''}
          </span>
        </li>
      </ul>
    </div>
  );
}

function CopyIcon() {
  return (
    <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
      <rect
        x="5.5"
        y="5.5"
        width="8"
        height="8"
        rx="1.5"
        fill="none"
        stroke="currentColor"
        stroke-width="1.3"
      />
      <path
        d="M10.5 3.5v-.5a1.5 1.5 0 0 0-1.5-1.5H4A1.5 1.5 0 0 0 2.5 3v5A1.5 1.5 0 0 0 4 9.5h.5"
        fill="none"
        stroke="currentColor"
        stroke-width="1.3"
      />
    </svg>
  );
}

function CheckIcon() {
  return (
    <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
      <path
        d="m3.5 8.5 3 3 6-7"
        fill="none"
        stroke="currentColor"
        stroke-width="1.5"
        stroke-linecap="round"
        stroke-linejoin="round"
      />
    </svg>
  );
}
