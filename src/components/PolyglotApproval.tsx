import { For, Show } from 'solid-js';
import type { HarnessApproval } from '../lib/polyglotHarness';

function label(key: string): string {
  const words = key.replace(/_/g, ' ').replace(/([a-z])([A-Z])/g, '$1 $2');
  return words.charAt(0).toUpperCase() + words.slice(1);
}

/** Render the actual reviewed artifact as labelled fields, preserving every value. */
function SubjectFields(props: { value: unknown }) {
  if (Array.isArray(props.value)) {
    return (
      <ul>
        <For each={props.value}>
          {(value) => (
            <li>
              <SubjectFields value={value} />
            </li>
          )}
        </For>
      </ul>
    );
  }
  if (props.value !== null && typeof props.value === 'object') {
    return (
      <dl>
        <For each={Object.entries(props.value)}>
          {([key, value]) => (
            <>
              <dt>
                <strong>{label(key)}</strong>
              </dt>
              <dd>
                <SubjectFields value={value} />
              </dd>
            </>
          )}
        </For>
      </dl>
    );
  }
  return (
    <span style={{ 'white-space': 'pre-wrap' }}>
      {props.value === null ? 'None' : String(props.value)}
    </span>
  );
}

/** Exact subject and bounded protected-file previews supplied by the controller. */
export default function PolyglotApproval(props: { approval: HarnessApproval }) {
  return (
    <div class="polyglot-approval-subject">
      <p>
        <strong>{label(props.approval.subjectKind || 'Approval subject')}</strong>
      </p>
      <Show
        when={props.approval.subject != null}
        fallback={
          <p role="alert">
            The approval subject could not be read. Refresh the result before approving.
          </p>
        }
      >
        <SubjectFields value={props.approval.subject} />
      </Show>
      <For each={props.approval.assets ?? []}>
        {(asset) => (
          <details>
            <summary>{asset.path}</summary>
            <p class="settings-hint">
              Content identity: <code>{asset.digest}</code>
            </p>
            <Show when={asset.content != null}>
              <pre style={{ 'white-space': 'pre-wrap', 'overflow-wrap': 'anywhere' }}>
                {asset.content}
              </pre>
            </Show>
            <Show when={asset.omitted}>
              <p class="settings-status warning">{asset.omitted}</p>
            </Show>
          </details>
        )}
      </For>
    </div>
  );
}
