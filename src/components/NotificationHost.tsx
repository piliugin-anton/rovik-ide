// NotificationHost.tsx: renders lib/notifications.ts, bottom right.
import { createMemo, For, Index, Show } from 'solid-js';
import { dismiss, notifications, type AppNotification } from '../lib/notifications';

export default function NotificationHost() {
  // By id: an update (a download's progress, a new message) makes a new
  // object, and a new element for it every time made its buttons, the
  // close button too, nearly impossible to click while it changed.
  const ids = createMemo(() => notifications().map((n) => n.id), [], {
    equals: (a, b) => a.length === b.length && a.every((id, i) => id === b[i]),
  });
  return (
    <div class="notifications" aria-live="polite">
      <For each={ids()}>
        {(id) => (
          <Show when={notifications().find((n) => n.id === id)}>
            {(note) => <Notification n={note()} />}
          </Show>
        )}
      </For>
    </div>
  );
}

function Notification(props: { n: AppNotification }) {
  const n = () => props.n;
  const percent = () => {
    const p = n().progress;
    return typeof p === 'number' ? Math.round(p * 100) : null;
  };
  return (
    <div class={`notification ${n().kind ?? 'info'}`} role="status">
      <div class="notification-head">
        <span class="notification-title">{n().title}</span>
        <button
          type="button"
          class="notification-close"
          aria-label="Dismiss"
          title="Dismiss"
          onClick={() => dismiss(n().id)}
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
      <Show when={n().message}>
        <div class="notification-message">{n().message}</div>
      </Show>
      <Show when={n().progress !== undefined}>
        <div
          class="notification-progress"
          classList={{ busy: n().progress === 'busy' }}
          role="progressbar"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={percent() ?? undefined}
        >
          <div
            class="notification-progress-fill"
            style={{ width: percent() === null ? undefined : `${percent()}%` }}
          />
        </div>
      </Show>
      <Show when={n().actions?.length}>
        <div class="notification-actions">
          <Index each={n().actions}>
            {(action) => (
              <button
                type="button"
                class="notification-action"
                classList={{ primary: action().primary }}
                onClick={() => {
                  if (!action().keepOpen) dismiss(n().id);
                  action().run();
                }}
              >
                {action().label}
              </button>
            )}
          </Index>
        </div>
      </Show>
    </div>
  );
}
