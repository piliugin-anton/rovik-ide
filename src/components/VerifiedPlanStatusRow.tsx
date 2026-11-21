import { Index, Show } from 'solid-js';

export interface PlanStatus {
  title: string;
  message?: string;
  busy?: boolean;
  attention?: boolean;
  onDismiss?: () => void;
  actions?: { label: string; primary?: boolean; run: () => void }[];
}

/** One inline status for the plan in the selected chat. */
export default function VerifiedPlanStatusRow(props: { status?: PlanStatus }) {
  return (
    <Show when={props.status}>
      {(status) => (
        <section
          class="verified-plan-status"
          classList={{ 'needs-attention': !!status().attention }}
          aria-label="Verified plan status"
        >
          <span role="status" class="verified-plan-label">
            <Show when={status().busy}>
              <span class="tool-call-spinner" aria-hidden="true" />
            </Show>
            {status().title}
          </span>
          <Show when={status().message}>
            {(message) => (
              <span class="verified-plan-message" aria-live="polite" tabIndex={0}>
                {message()}
              </span>
            )}
          </Show>
          <div class="verified-plan-actions">
            <Index each={status().actions}>
              {(action) => (
                <button
                  type="button"
                  class={action().primary ? 'chat-action-button primary' : 'link-button'}
                  onClick={() => action().run()}
                >
                  {action().label}
                </button>
              )}
            </Index>
            <Show when={status().onDismiss}>
              {(close) => (
                <button
                  type="button"
                  class="notification-close"
                  aria-label="Close plan status"
                  onClick={() => close()()}
                >
                  ×
                </button>
              )}
            </Show>
          </div>
        </section>
      )}
    </Show>
  );
}
