import { createUniqueId, Show, type JSX } from 'solid-js';

/** A bounded chat review: long evidence scrolls without hiding its controls or the conversation. */
export default function PolyglotReviewFrame(props: {
  compact?: boolean;
  title: string;
  label: string;
  closeLabel: string;
  closeDisabled?: boolean;
  onClose: () => void;
  actions: JSX.Element;
  children: JSX.Element;
}) {
  const heading = createUniqueId();
  const CloseButton = () => (
    <button
      type="button"
      class="link-button"
      disabled={props.closeDisabled}
      onClick={props.onClose}
    >
      {props.closeLabel}
    </button>
  );
  return (
    <section
      class="polyglot-review"
      classList={{ compact: props.compact }}
      aria-label={props.label}
    >
      <header class="polyglot-review-header">
        <h3 id={heading}>{props.title}</h3>
        <Show when={props.compact}>
          <CloseButton />
        </Show>
      </header>
      <div class="polyglot-review-body" role="region" aria-labelledby={heading} tabIndex={0}>
        {props.children}
      </div>
      <footer class="polyglot-review-actions">
        <Show when={!props.compact}>
          <CloseButton />
        </Show>
        {props.actions}
      </footer>
    </section>
  );
}
