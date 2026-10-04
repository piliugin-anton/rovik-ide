// TodoPanel.tsx
//
// The agent's checklist while it works (lib/todo.ts), docked above the
// input like the question panel. Read-only: it shows what the agent is
// doing, it isn't a list to edit. Collapses to the current step.
import { createSignal, For, Show } from 'solid-js';
import { isSettled, progressLabel, type TodoItem } from '../lib/todo';
import { requestOpenFile } from '../lib/editorBridge';

const COLLAPSED_KEY = 'rovik.todoPanelCollapsed';

export function StatusIcon(props: { status: TodoItem['status'] }) {
  const common = {
    width: 13,
    height: 13,
    viewBox: '0 0 16 16',
    fill: 'none',
    stroke: 'currentColor',
    'stroke-width': 1.6,
    'stroke-linecap': 'round' as const,
    'stroke-linejoin': 'round' as const,
    'aria-hidden': true,
  };
  switch (props.status) {
    case 'done':
      return (
        <svg {...common} class="todo-icon done">
          <circle cx="8" cy="8" r="6.25" />
          <path d="M5.3 8.2 7.2 10l3.5-4" />
        </svg>
      );
    case 'in_progress':
      return (
        <svg {...common} class="todo-icon in_progress">
          <circle cx="8" cy="8" r="6.25" opacity="0.35" />
          <path d="M8 1.75a6.25 6.25 0 0 1 6.25 6.25" />
        </svg>
      );
    case 'skipped':
      return (
        <svg {...common} class="todo-icon skipped">
          <circle cx="8" cy="8" r="6.25" />
          <path d="M5.5 8h5" />
        </svg>
      );
    case 'changed':
      return (
        <svg {...common} class="todo-icon changed">
          <circle cx="8" cy="8" r="6.25" />
          <path d="M5.5 9.5 9.5 5.5M8 5.5h1.5V7" />
        </svg>
      );
    default:
      return (
        <svg {...common} class="todo-icon pending">
          <circle cx="8" cy="8" r="6.25" />
        </svg>
      );
  }
}

export default function TodoPanel(props: { items: TodoItem[]; plan?: string }) {
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

  const current = () =>
    props.items.find((i) => i.status === 'in_progress') ?? props.items.find((i) => !isSettled(i));
  const done = () => props.items.filter(isSettled).length;

  return (
    <div
      class="todo-panel"
      classList={{ collapsed: collapsed() }}
      role="status"
      aria-label="The agent's checklist"
    >
      <button type="button" class="todo-panel-head" onClick={toggle} aria-expanded={!collapsed()}>
        <svg
          class="todo-panel-caret"
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
        <span class="todo-panel-title">
          <Show
            when={collapsed() && current()}
            fallback={props.plan ? 'Implementing the plan' : 'Checklist'}
          >
            {(c) => c().title}
          </Show>
        </span>
        <span class="todo-panel-progress">{progressLabel(props.items)}</span>
        <span class="todo-panel-bar" aria-hidden="true">
          <span style={{ width: `${(done() / Math.max(1, props.items.length)) * 100}%` }} />
        </span>
      </button>
      <Show when={!collapsed()}>
        <Show when={props.plan}>
          {(plan) => (
            <button
              type="button"
              class="link-button todo-panel-plan"
              onClick={() => requestOpenFile({ path: plan() })}
              title="Open the plan"
            >
              {plan()}
            </button>
          )}
        </Show>
        <ol class="todo-list">
          <For each={props.items}>
            {(item) => (
              <li class={`todo-item ${item.status}`}>
                <StatusIcon status={item.status} />
                <span class="todo-item-text">
                  <span class="todo-item-title">{item.title}</span>
                  <Show when={item.note}>
                    <span class="todo-item-note">{item.note}</span>
                  </Show>
                </span>
              </li>
            )}
          </For>
        </ol>
      </Show>
    </div>
  );
}
