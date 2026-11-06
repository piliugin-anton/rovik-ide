// PriorWorkPanel.tsx
//
// "Relevant prior work": the project's long-term memories this run recalled
// (the memoryRecalled event, lib/memory.ts), what became of them once the
// checks ran — promoted, still provisional, invalidated — and whether the
// run's outcome says the approach helped. Read-only: the agent writes the
// memory, the user reads it here.
import { For, Show } from 'solid-js';
import { memoryStateLabel, type RecalledMemory } from '../lib/memory';

export default function PriorWorkPanel(props: { memories: RecalledMemory[] }) {
  /** What the run's outcome said of the memories it used; absent: no verdict. */
  const verdict = () => {
    const helped = props.memories.find((m) => m.helped !== undefined)?.helped;
    if (helped === true) return 'This approach helped';
    if (helped === false) return 'This approach did not help';
    return undefined;
  };

  return (
    <section class="prior-work" aria-label="Relevant prior work">
      <div class="prior-work-head">
        <span class="prior-work-title">Relevant prior work</span>
        <Show when={verdict()}>{(text) => <span class="prior-work-verdict">{text()}</span>}</Show>
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
