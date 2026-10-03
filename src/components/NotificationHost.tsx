// NotificationHost.tsx: renders lib/notifications.ts, bottom right.
import { For, Show } from 'solid-js';
import { dismiss, notifications } from '../lib/notifications';

export default function NotificationHost() {
  return (
    <div class="notifications" aria-live="polite">
      <For each={notifications()}>
        {(n) => (
          <div class={`notification ${n.kind ?? 'info'}`} role="status">
            <div class="notification-head">
              <span class="notification-title">{n.title}</span>
              <button
                type="button"
                class="notification-close"
                aria-label="Dismiss"
                title="Dismiss"
                onClick={() => dismiss(n.id)}
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
            <Show when={n.message}>
              <div class="notification-message">{n.message}</div>
            </Show>
            <Show when={n.progress !== undefined}>
              <div
                class="notification-progress"
                classList={{ busy: n.progress === 'busy' }}
                role="progressbar"
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={
                  typeof n.progress === 'number' ? Math.round(n.progress * 100) : undefined
                }
              >
                <div
                  class="notification-progress-fill"
                  style={{
                    width:
                      typeof n.progress === 'number'
                        ? `${Math.round(n.progress * 100)}%`
                        : undefined,
                  }}
                />
              </div>
            </Show>
            <Show when={n.actions?.length}>
              <div class="notification-actions">
                <For each={n.actions}>
                  {(action) => (
                    <button
                      type="button"
                      class="notification-action"
                      classList={{ primary: action.primary }}
                      onClick={() => {
                        if (!action.keepOpen) dismiss(n.id);
                        action.run();
                      }}
                    >
                      {action.label}
                    </button>
                  )}
                </For>
              </div>
            </Show>
          </div>
        )}
      </For>
    </div>
  );
}
