// MoveChatDialog.tsx
//
// Moving a chat to a host, or back (U02, 6.8). Before a move there, the
// preflight says what moves, what will wait for you there (the permissions
// with the host's approval policy), whether its CLM context becomes
// trimming, that the server keeps your API keys, and what the project's
// push carries that you'd want to know; a chat across a workspace's folders
// lists them, whose git history stays here (workspaces-plan.md, 4.11); a
// host whose Rovik is too old to take chats is upgraded for everyone from
// here by its administrators (R02). Then the move's steps as `cloud-event`
// tells them (lib/cloud.ts), with "Cancel move" while it goes, and how it
// ended.
import { createResource, createSignal, For, Match, onMount, Show, Switch } from 'solid-js';
import { Portal } from 'solid-js/web';
import type { StartRequest } from '../lib/agentRuns';
import {
  cancelMove,
  cloud,
  hostName,
  moveEnded,
  movePreflight,
  moveStepLabel,
  startMove,
  type MovePreflight,
} from '../lib/cloud';
import { asDeployFailure, upgradeHost } from '../lib/hosts';
import { formatBytes, plural } from '../lib/textUtils';

/** What the move dialog says of a server's keeping your keys (design §6). */
export const KEYS_NOTICE =
  'Your API keys are stored in your account on this server, so chats there run while this computer is away. The agent’s commands there and anyone with root on it can read them.';

export default function MoveChatDialog(props: {
  dialogId: string;
  chatTitle: string;
  /** "there": to `host`; "back": from it. */
  direction: 'there' | 'back';
  /** The host's id. */
  host: string;
  /** How a Continue would start the chat: its run picks up with it after the move. */
  startFor: () => Promise<StartRequest | null>;
  onClose: () => void;
}) {
  const there = props.direction === 'there';
  const name = () => hostName(props.host, cloud.hosts);
  const title = () =>
    there
      ? `Move “${props.chatTitle}” to ${name()}`
      : `Move “${props.chatTitle}” back to this computer`;

  // Asked again as the host's connection changes: back on a new Rovik
  // after an upgrade, it may take the chat.
  const hostState = () => cloud.hosts.find((h) => h.id === props.host)?.state ?? '';
  const [preflight] = createResource(
    () => there && `${props.dialogId}\n${hostState()}`,
    () => movePreflight(props.dialogId, props.host)
  );
  const [started, setStarted] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const move = () => (started() ? cloud.moves[props.dialogId] : undefined);
  const going = () => started() && !moveEnded(move());
  const canMove = () => {
    if (!there) return true;
    const p = preflight.state === 'ready' ? preflight() : undefined;
    return !!p && !p.blocked;
  };

  const offersUpgrade = () => {
    const p = preflight.state === 'ready' ? preflight() : undefined;
    return !!p?.blocked && !!p.upgrade;
  };
  const [upgrading, setUpgrading] = createSignal(false);
  async function upgrade() {
    setError(null);
    setUpgrading(true);
    try {
      await upgradeHost(props.host);
    } catch (err) {
      setError(asDeployFailure(err).message);
    } finally {
      setUpgrading(false);
    }
  }

  const [preparing, setPreparing] = createSignal(false);
  async function go() {
    setError(null);
    setPreparing(true);
    let start: StartRequest | null;
    try {
      start = await props.startFor();
    } catch (err) {
      setError(String(err));
      return;
    } finally {
      setPreparing(false);
    }
    setStarted(true);
    // Its last step says why, if it fails.
    await startMove(props.dialogId, props.direction, props.host, start).catch(() => {});
  }

  async function cancel() {
    setError(null);
    try {
      await cancelMove(props.dialogId);
    } catch (err) {
      setError(String(err));
    }
  }

  // While it goes, closing only hides the dialog: the move goes on.
  const close = () => props.onClose();
  let first: HTMLButtonElement | undefined;
  onMount(() => first?.focus());

  return (
    <Portal>
      <div
        class="modal-overlay"
        onMouseDown={(e) => {
          if (e.target === e.currentTarget) close();
        }}
      >
        <div
          class="modal confirm-dialog move-chat-dialog"
          role="dialog"
          aria-modal="true"
          aria-label={title()}
          onKeyDown={(e) => {
            if (e.key === 'Escape') {
              e.preventDefault();
              e.stopPropagation();
              close();
            }
          }}
        >
          <div class="confirm-dialog-body">
            <div class="confirm-dialog-title">{title()}</div>
            <Show
              when={started()}
              fallback={
                there ? (
                  <Preflight state={preflight.state} value={preflight()} error={preflight.error} />
                ) : (
                  <div class="confirm-dialog-message">
                    The chat comes back with its messages. Its changes on {name()} come with it and
                    wait for you to review and land them, as an isolated chat’s do. A run going
                    there pauses at a safe point and picks up here. Once {name()} lets the chat go,
                    the move can’t be cancelled.
                  </div>
                )
              }
            >
              <Show when={move()}>
                {(m) => (
                  <div class="move-chat-progress" role="status">
                    <div
                      class="move-chat-step"
                      classList={{
                        failed: m().step === 'failed',
                        done: m().step === 'moved',
                      }}
                    >
                      {moveStepLabel(m())}
                      <Show when={m().step === 'pushing' && m().pushed}>
                        {(sent) => <span class="move-chat-bytes"> {formatBytes(sent())} sent</span>}
                      </Show>
                    </div>
                    <Show when={m().message}>
                      <div class="move-chat-reason">{m().message}</div>
                    </Show>
                    <Show when={!moveEnded(m())}>
                      <div class="confirm-dialog-message">
                        You can close this: the move goes on, and the chat is read-only until it
                        ends.
                      </div>
                    </Show>
                  </div>
                )}
              </Show>
            </Show>
            <Show when={error()}>
              <div class="move-chat-reason">{error()}</div>
            </Show>
          </div>
          <div class="confirm-dialog-actions">
            <Switch>
              <Match when={!started()}>
                <button ref={first} type="button" onClick={close}>
                  Cancel
                </button>
                <Show when={offersUpgrade()}>
                  <button type="button" disabled={upgrading()} onClick={() => void upgrade()}>
                    {upgrading() ? 'Upgrading…' : 'Upgrade for everyone'}
                  </button>
                </Show>
                <button
                  type="button"
                  class="primary"
                  disabled={!canMove() || preparing()}
                  onClick={() => void go()}
                >
                  {there ? 'Move' : 'Move back'}
                </button>
              </Match>
              <Match when={going()}>
                <button type="button" onClick={() => void cancel()}>
                  Cancel move
                </button>
                <button type="button" onClick={close}>
                  Close
                </button>
              </Match>
              <Match when={true}>
                <button type="button" class="primary" onClick={close}>
                  Close
                </button>
              </Match>
            </Switch>
          </div>
        </div>
      </div>
    </Portal>
  );
}

/** Whether the preflight is a chat's across a workspace's folders. */
const across = (p: MovePreflight) => (p.folders?.length ?? 0) > 0;

/** Before a move there: what it does, or why it can't happen now. */
function Preflight(props: {
  state: 'unresolved' | 'pending' | 'ready' | 'refreshing' | 'errored';
  value: MovePreflight | undefined;
  error: unknown;
}) {
  return (
    <Switch fallback={<div class="confirm-dialog-message">Checking what the move takes…</div>}>
      <Match when={props.state === 'errored'}>
        <div class="move-chat-reason">{String(props.error)}</div>
      </Match>
      <Match when={props.value?.blocked}>
        {(why) => <div class="move-chat-reason">{why()}</div>}
      </Match>
      <Match when={props.value}>
        {(p) => (
          <div class="move-chat-preflight">
            <div class="confirm-dialog-message">
              This chat moves with its messages and its subagents’ chats, and{' '}
              {across(p())
                ? 'its folders go too, as they are now, with this chat’s changes in them'
                : 'the project goes too, as it is now, with this chat’s changes in it'}
              {p().files > 0
                ? `, as do ${plural(p().files, 'file')} its messages name (${formatBytes(p().fileBytes)})`
                : ''}
              . A run going now pauses at a safe point and picks up there.
            </div>
            <Show when={across(p())}>
              <div class="move-chat-section">Its folders</div>
              <ul class="move-chat-list move-chat-folders">
                <For each={p().folders}>{(folder) => <li>{folder}</li>}</For>
              </ul>
              <div class="confirm-dialog-message move-chat-note">
                Their git history stays on this computer: the first push carries all their files.
              </div>
            </Show>
            <div class="move-chat-section">What will wait for you there</div>
            <Show
              when={p().waits.length > 0}
              fallback={
                <div class="confirm-dialog-message">
                  Nothing: with your permissions and this server’s approval policy, its runs don’t
                  stop for you.
                </div>
              }
            >
              <ul class="move-chat-list">
                <For each={p().waits}>{(wait) => <li>{wait}</li>}</For>
              </ul>
            </Show>
            <Show when={p().trimming}>
              <div class="confirm-dialog-message move-chat-note">
                Its context becomes trimming there: the server has no virtual files for CLM.
              </div>
            </Show>
            <Show when={p().redoLost}>
              <div class="confirm-dialog-message move-chat-note">
                “Redo sequentially” for its parallel group won’t be offered after the move.
              </div>
            </Show>
            <Show when={p().notes.length > 0}>
              <div class="move-chat-section">
                {across(p()) ? 'About its folders' : 'About the project'}
              </div>
              <ul class="move-chat-list">
                <For each={p().notes}>{(note) => <li>{note}</li>}</For>
              </ul>
            </Show>
            <div class="confirm-dialog-message move-chat-keys">{KEYS_NOTICE}</div>
          </div>
        )}
      </Match>
    </Switch>
  );
}
