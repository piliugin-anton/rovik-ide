// HostChatStorage.tsx
//
// A cloud host's page, connected: what each of this computer's chats'
// working copies takes there, the largest first, by kind (source,
// installed dependencies, build output, other files git ignores), and the
// optional storage budget that removes idle chats' dependencies and build
// output, but those kept ready.
import { createEffect, createSignal, For, on, Show } from 'solid-js';
import type { ChatStorage } from '../lib/hosts';
import { formatBytes } from '../lib/textUtils';

const GB = 1024 ** 3;

/** What a chat's working copy takes in all. */
const total = (c: ChatStorage) => c.source + c.dependencies + c.build + c.other;

export default function HostChatStorage(props: {
  chats: ChatStorage[];
  /** The budget set there, in bytes. */
  budget?: number;
  /** Resolves whether the server took it: the box goes back when not. */
  onKeepReady: (chat: string, keep: boolean) => Promise<boolean>;
  /** Sets the budget, in bytes, or none. */
  onBudget: (bytes: number | null) => void;
}) {
  const shown = (bytes?: number) => (bytes ? String(+(bytes / GB).toFixed(2)) : '');
  const [budget, setBudget] = createSignal(shown(props.budget));
  // The server's budget, when it changes (set from another computer).
  createEffect(
    on(
      () => props.budget,
      (bytes) => setBudget(shown(bytes)),
      { defer: true }
    )
  );
  /** The budget typed, in bytes: null for none, undefined when it isn't a size of 1 GB or more. */
  const typed = () => {
    const text = budget().trim();
    if (!text) return null;
    const gb = Number(text);
    return Number.isFinite(gb) && gb >= 1 ? Math.round(gb * GB) : undefined;
  };
  return (
    <div class="host-chat-storage">
      <Show when={props.chats.length}>
        <div class="host-chat-storage-scroll">
          <table class="host-chat-storage-table">
            <thead>
              <tr>
                <th>Chat</th>
                <th>Total</th>
                <th>Source</th>
                <th>Dependencies</th>
                <th>Build output</th>
                <th>Other files</th>
                <th>Keep ready</th>
              </tr>
            </thead>
            <tbody>
              <For each={props.chats}>
                {(chat) => (
                  <tr>
                    <td class="host-chat-storage-title">
                      {chat.title || chat.chat}
                      <Show when={chat.running}>
                        <span class="host-task-running">Running</span>
                      </Show>
                    </td>
                    <td>{formatBytes(total(chat))}</td>
                    <td>{formatBytes(chat.source)}</td>
                    <td>{formatBytes(chat.dependencies)}</td>
                    <td>{formatBytes(chat.build)}</td>
                    <td>{formatBytes(chat.other)}</td>
                    <td>
                      <input
                        type="checkbox"
                        checked={chat.keepReady}
                        aria-label={`Keep ${chat.title || chat.chat} ready`}
                        onChange={(e) => {
                          const box = e.currentTarget;
                          const keep = box.checked;
                          void props.onKeepReady(chat.chat, keep).then((taken) => {
                            if (!taken) box.checked = !keep;
                          });
                        }}
                      />
                    </td>
                  </tr>
                )}
              </For>
            </tbody>
          </table>
        </div>
      </Show>
      <div class="host-limit host-storage-budget">
        <label for="host-storage-budget">Budget for chats’ working copies (GB)</label>
        <input
          id="host-storage-budget"
          type="text"
          inputmode="decimal"
          placeholder="None"
          value={budget()}
          onInput={(e) => setBudget(e.currentTarget.value)}
        />
        <button
          disabled={typed() === undefined}
          onClick={() => {
            const bytes = typed();
            if (bytes !== undefined) props.onBudget(bytes);
          }}
        >
          Save budget
        </button>
      </div>
      <p class="settings-hint">
        Over the budget, the dependencies and build output of chats idle for an hour go, those idle
        longest first, checked each hour. A chat kept ready keeps them, here and with “Clean up
        now”.
      </p>
    </div>
  );
}
