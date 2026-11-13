import { createUniqueId, type JSX } from 'solid-js';

/** A bounded chat review: long evidence scrolls without hiding its controls or the conversation. */
export default function PolyglotReviewFrame(props: {
  title: string;
  label: string;
  closeLabel: string;
  closeDisabled?: boolean;
  onClose: () => void;
  actions: JSX.Element;
  children: JSX.Element;
}) {
  const heading = createUniqueId();
  return (
    <section class="polyglot-review" aria-label={props.label}>
      <header class="polyglot-review-header">
        <h3 id={heading}>{props.title}</h3>
      </header>
      <div class="polyglot-review-body" role="region" aria-labelledby={heading} tabIndex={0}>
        {props.children}
      </div>
      <footer class="polyglot-review-actions">
        <button
          type="button"
          class="link-button"
          disabled={props.closeDisabled}
          onClick={props.onClose}
        >
          {props.closeLabel}
        </button>
        {props.actions}
      </footer>
    </section>
  );
}
