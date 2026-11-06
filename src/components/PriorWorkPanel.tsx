// PriorWorkPanel.tsx
//
// "Relevant prior work": the project's long-term memories this run recalled
// (the memoryRecalled event, lib/memory.ts), what became of them once the
// checks ran — promoted, still provisional, invalidated — whether they still
// apply, and how the run that carried them ended. Read-only: the agent writes
// the memory, the user reads it here.
import { For, Show } from 'solid-js';
import { memoryApplicabilityLabel, memoryStateLabel, type RecalledMemory } from '../lib/memory';

export default function PriorWorkPanel(props: { memories: RecalledMemory[] }) {
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
    <section class="prior-work" aria-label="Relevant prior work">
      <div class="prior-work-head">
        <span class="prior-work-title">Relevant prior work</span>
        <Show when={association()}>
          {(text) => <span class="prior-work-verdict">{text()}</span>}
        </Show>
      </div>
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
    </section>
  );
}
