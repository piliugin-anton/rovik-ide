import { createMemo, createSignal, Show } from 'solid-js';
import { summarizeChatError } from '../lib/chatErrors';

/** A failure belongs to the IDE, even when it is recorded on the user's request. */
export default function ChatFailureNotice(props: {
  title: string;
  message: string;
  details?: string;
  onRetry?: () => void;
}) {
  const [expanded, setExpanded] = createSignal(false);
  const summary = createMemo(() => summarizeChatError(props.message));
  const details = () => props.details || (summary() !== props.message ? props.message : undefined);
  return (
    <section class="chat-failure" aria-label="Chat error">
      <strong class="chat-failure-title">{props.title}</strong>
      <p class="chat-failure-summary">{summary()}</p>
      <div class="chat-failure-actions">
        <Show when={props.onRetry}>
          {(retry) => (
            <button type="button" class="chat-action-button" onClick={() => retry()()}>
              Retry
            </button>
          )}
        </Show>
        <Show when={details()}>
          <button
            type="button"
            class="link-button"
            aria-expanded={expanded()}
            onClick={() => setExpanded(!expanded())}
          >
            {expanded() ? 'Hide details' : 'Details'}
          </button>
        </Show>
      </div>
      <Show when={expanded() && details()}>
        {(output) => (
          <pre class="chat-failure-details" aria-label="Error details" tabIndex={0}>
            {output()}
          </pre>
        )}
      </Show>
    </section>
  );
}
