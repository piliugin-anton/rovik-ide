// PriorWorkPanel.tsx
//
// "Relevant prior work": the project's long-term memories this run recalled
// (the memoryRecalled event, lib/memory.ts), what became of them once the
// checks ran — promoted, still provisional, invalidated — whether they still
// apply, and how the run that carried them ended. Read-only: the agent writes
// the memory, the user reads it here.
import { createSignal, For, Show } from 'solid-js';
import { memoryApplicabilityLabel, memoryStateLabel, type RecalledMemory } from '../lib/memory';

const COLLAPSED_KEY = 'rovik.priorWorkCollapsed';

export default function PriorWorkPanel(props: { memories: RecalledMemory[] }) {
  // Folded to its header, remembered across chats and runs: the chat asks for
  // the panel again on every `memoryRecalled` event, so a fold kept only in
  // this component would spring open mid-run.
  const stored = (() => {
    try {
      return localStorage.getItem(COLLAPSED_KEY) === '1';
    } catch {
      return false;
    }
  })();
  const [collapsed, setCollapsed] = createSignal(stored);
  const toggle = () => {
    const next = !collapsed();
    setCollapsed(next);
    try {
      localStorage.setItem(COLLAPSED_KEY, next ? '1' : '0');
    } catch {
      // not remembered
    }
  };

  /**
   * How the run that had these in hand ended. Said as an association, not a
   * verdict: the checks passing says something happened in a run whose prompt
   * carried the memories, not that any of them helped.
   */
  const association = () => {
    const ended = props.memories.find((m) => m.association !== undefined)?.association;
    if (ended === 'checksPassed') return 'The checks passed after this';
    if (ended === 'checksFailed') return 'The checks failed after this';
    return undefined;
  };

  return (
    <section
      class="prior-work"
      classList={{ collapsed: collapsed() }}
      aria-label="Relevant prior work"
    >
      <button type="button" class="prior-work-head" onClick={toggle} aria-expanded={!collapsed()}>
        <svg
          class="prior-work-caret"
          classList={{ open: !collapsed() }}
          width="10"
          height="10"
          viewBox="0 0 16 16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.6"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M6 4l4 4-4 4" />
        </svg>
        <span class="prior-work-title">Relevant prior work</span>
        <Show when={association()}>
          {(text) => <span class="prior-work-verdict">{text()}</span>}
        </Show>
      </button>
      <Show when={!collapsed()}>
        <ul class="prior-work-list">
          <For each={props.memories}>
            {(memory) => (
              <li class="prior-work-item">
                <span
                  class="prior-work-state"
                  classList={{ [memory.verification]: true }}
                  title={memory.evidence}
                >
                  {memoryStateLabel(memory.verification)}
                </span>
                <Show when={memoryApplicabilityLabel(memory.applicability)}>
                  {(label) => (
                    <span
                      class="prior-work-applicability"
                      classList={{ [memory.applicability ?? 'unknown']: true }}
                      title="Whether this still holds for the code as it is now"
                    >
                      {label()}
                    </span>
                  )}
                </Show>
                <span class="prior-work-kind">{memory.kind}</span>
                <span class="prior-work-memory-title">{memory.title}</span>
                <span class="prior-work-content">{memory.content}</span>
                <Show when={memory.files?.length}>
                  <span class="prior-work-files">{memory.files!.join(', ')}</span>
                </Show>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </section>
  );
}
