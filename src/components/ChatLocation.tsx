// ChatLocation.tsx
//
// Where the open chat is (U02): a chip in the chat's header, "This
// computer", its host's name (offline, when it is), or the move going on.
// Pressed, it opens the chat's menu: "Move to <host>…" for each deployed
// host, "Move back to this computer…" and "Re-sync this chat" for a chat on
// a host, "Cancel move" while one goes. Shown once there's a host, or the
// chat is on one.
import { createEffect, createSignal, For, Match, onCleanup, Show, Switch } from 'solid-js';
import ContextMenuItem from './ContextMenuItem';
import MoveChatDialog from './MoveChatDialog';
import type { StartRequest } from '../lib/agentRuns';
import { openAppSettings } from '../lib/appSettings';
import { chatApi } from '../lib/chatApi';
import { cancelMove, cloud, placeLabel, type HostSummary } from '../lib/cloud';
import { notify } from '../lib/notifications';
import type { DialogSummary } from '../types';

/** A move the menu opened the dialog for, bound to its chat. */
interface Opened {
  dialogId: string;
  title: string;
  direction: 'there' | 'back';
  host: string;
}

/** Why a host can't take a move or a re-sync now, if it can't. */
function unavailable(host: HostSummary | undefined): string | null {
  switch (host?.state) {
    case 'ready':
      return null;
    case 'offline':
      return 'offline';
    case 'version-mismatch':
      return 'needs a newer app';
    case 'refused':
      return 'refused the connection';
    default:
      return 'connecting';
  }
}

export default function ChatLocation(props: {
  dialog: DialogSummary;
  /**
   * How a Continue would start the chat, once what it queued is stored: its
   * run picks up with it after the move.
   */
  startFor: (dialogId: string) => Promise<StartRequest | null>;
  /** After "Re-sync this chat": its messages, loaded again. */
  onResynced: (dialogId: string) => void;
}) {
  const place = () => props.dialog.place;
  const label = () => placeLabel(place(), cloud.hosts);
  const hostOf = (id: string) => cloud.hosts.find((h) => h.id === id);
  const [menu, setMenu] = createSignal<{ x: number; y: number } | null>(null);
  const [opened, setOpened] = createSignal<Opened | null>(null);
  let chip: HTMLButtonElement | undefined;
  let menuEl: HTMLDivElement | undefined;
  const close = () => setMenu(null);

  function toggle() {
    if (menu()) return close();
    const r = chip!.getBoundingClientRect();
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

  // Closed by a press elsewhere, a key, or the window losing focus or size,
  // as the app's other menus are.
  createEffect(() => {
    if (!menu()) return;
    const outside = (e: Event) => {
      const target = e.target as Node;
      if (!menuEl?.contains(target) && !chip?.contains(target)) close();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') e.preventDefault();
      close();
    };
    window.addEventListener('pointerdown', outside, true);
    window.addEventListener('keydown', onKey, true);
    window.addEventListener('blur', close);
    window.addEventListener('resize', close);
    onCleanup(() => {
      window.removeEventListener('pointerdown', outside, true);
      window.removeEventListener('keydown', onKey, true);
      window.removeEventListener('blur', close);
      window.removeEventListener('resize', close);
    });
  });

  function openMove(direction: 'there' | 'back', host: string) {
    close();
    setOpened({ dialogId: props.dialog.id, title: props.dialog.title, direction, host });
  }

  async function resync() {
    close();
    const id = props.dialog.id;
    try {
      await chatApi.resyncDialog(id);
      props.onResynced(id);
    } catch (err) {
      notify({
        kind: 'error',
        title: 'The chat couldn’t be re-synced',
        message: String(err),
        timeout: 12_000,
      });
    }
  }

  async function cancel() {
    close();
    try {
      await cancelMove(props.dialog.id);
    } catch (err) {
      notify({
        kind: 'error',
        title: 'The move couldn’t be cancelled',
        message: String(err),
        timeout: 12_000,
      });
    }
  }

  /** "Move to <host>…", and why not now when the host can't take it. */
  const moveLabel = (host: HostSummary) => {
    const why = unavailable(host);
    return why ? `Move to ${host.name}… (${why})` : `Move to ${host.name}…`;
  };

  return (
    <Show when={cloud.hosts.length > 0 || place()}>
      <button
        ref={chip}
        type="button"
        class={`chat-location ${label().tone}`}
        title={label().title}
        aria-haspopup="menu"
        aria-expanded={!!menu()}
        onClick={toggle}
      >
        <span class="chat-location-dot" aria-hidden="true" />
        <span class="chat-location-label">{label().label}</span>
        <span class="chat-location-caret" aria-hidden="true">
          ▾
        </span>
      </button>
      <Show when={menu()}>
        {(at) => (
          <div
            ref={placeMenu}
            class="context-menu chat-location-menu"
            style={{ top: `${at().y}px`, left: `${at().x}px` }}
            onContextMenu={(e) => e.preventDefault()}
          >
            <ul class="context-menu-list" role="menu" aria-label="Chat">
              <Switch>
                <Match when={!place()}>
                  <For
                    each={cloud.hosts.filter((h) => h.deployed && !h.wsl)}
                    fallback={
                      <ContextMenuItem
                        label="Set up a cloud host…"
                        onClick={() => {
                          close();
                          openAppSettings('hosts');
                        }}
                      />
                    }
                  >
                    {(host) => (
                      <ContextMenuItem
                        label={moveLabel(host)}
                        disabled={!!unavailable(host)}
                        onClick={() => openMove('there', host.id)}
                      />
                    )}
                  </For>
                </Match>
                <Match when={place()?.kind === 'host' && place()}>
                  {(on) => (
                    <>
                      <ContextMenuItem
                        label="Move back to this computer…"
                        disabled={!!unavailable(hostOf(on().host))}
                        onClick={() => openMove('back', on().host)}
                      />
                      <ContextMenuItem
                        label="Re-sync this chat"
                        disabled={!!unavailable(hostOf(on().host))}
                        onClick={() => void resync()}
                      />
                    </>
                  )}
                </Match>
                <Match when={true}>
                  <ContextMenuItem label="Cancel move" onClick={() => void cancel()} />
                </Match>
              </Switch>
            </ul>
          </div>
        )}
      </Show>
      <Show when={opened()}>
        {(o) => (
          <MoveChatDialog
            dialogId={o().dialogId}
            chatTitle={o().title}
            direction={o().direction}
            host={o().host}
            startFor={() => props.startFor(o().dialogId)}
            onClose={() => setOpened(null)}
          />
        )}
      </Show>
    </Show>
  );
}
