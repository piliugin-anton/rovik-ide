// ConnectorRecord.tsx
//
// What a connector's harness did for a reply (lib/connectors.ts
// ConnectorMeta), as a line like a tool call's: "Claude Code ·
// claude-sonnet-5 · 7 steps · 12.3k tokens · $0.04", open to each step
// (the tool it ran, what with, and the first lines of its result). Updates
// live while the harness works: the steps arrive as it runs them, the
// tokens and the cost with its result. A step that waits for the user's
// approval (its runs follow Rovik's permissions) shows the request under
// the line, open or not, as a tool call of Rovik's does.
import { createSignal, For, Show } from 'solid-js';
import {
  connectorHeadline,
  stepLabel,
  type ConnectorMeta,
  type HarnessStep,
} from '../lib/connectors';
import { pendingApprovalsOf } from '../lib/approvals';
import { ApprovalBar } from './ToolCallBlock';

interface RecordProps {
  meta: ConnectorMeta;
  running: boolean;
  /** The chat it's in: its steps' approvals wait there. */
  dialogId?: string | null;
}

export default function ConnectorRecord(props: RecordProps) {
  // With Rovik's tools, the reply's calls show as Rovik's own: the record
  // only says what it used, once it's known.
  return (
    <Show when={!props.meta.rovikTools || props.meta.usage}>
      <Record {...props} />
    </Show>
  );
}

function Record(props: RecordProps) {
  const [open, setOpen] = createSignal(false);
  const steps = () => props.meta.steps ?? [];
  /** The approvals its steps wait for. */
  const waiting = () => {
    const ids = new Set(steps().map((s) => s.id));
    return props.dialogId
      ? pendingApprovalsOf(props.dialogId).filter((r) => ids.has(r.toolCallId))
      : [];
  };
  const working = () => props.running && !props.meta.usage;
  const failed = () => steps().filter((s) => s.ok === false).length;
  const denied = () => props.meta.usage?.connector?.permissionDenials ?? 0;
  const lastStep = () => {
    const all = steps();
    return all.length ? stepLabel(all[all.length - 1]) : '';
  };
  return (
    <div class="tool-group chat-connector" classList={{ open: open(), active: working() }}>
      <div class="tool-group-header" onClick={() => setOpen((o) => !o)}>
        <button type="button" class="tool-group-toggle" aria-expanded={open()}>
          <svg
            class="chat-pasted-caret"
            classList={{ open: open() }}
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
          <span class="chat-connector-headline">{connectorHeadline(props.meta, working())}</span>
        </button>
        <Show when={working() && !open() && lastStep()}>
          <span class="tool-group-items chat-thinking-preview">{lastStep()}</span>
        </Show>
        <Show when={failed() || denied()}>
          <span class="tool-group-meta error">
            {[failed() ? `${failed()} failed` : '', denied() ? `${denied()} denied` : '']
              .filter(Boolean)
              .join(' · ')}
          </span>
        </Show>
        <Show when={working()}>
          <span class="tool-call-spinner" aria-label="Working" />
        </Show>
      </div>
      <For each={waiting()}>
        {(request) => (
          <div class="chat-connector-approval">
            <div class="chat-connector-approval-what">{request.summary}</div>
            <ApprovalBar request={request} />
          </div>
        )}
      </For>
      <Show when={open()}>
        <div class="tool-group-calls chat-connector-steps">
          <Show when={steps().length === 0}>
            <div class="chat-connector-empty">
              {working() ? 'No steps yet.' : 'It answered without running a tool.'}
            </div>
          </Show>
          <For each={steps()}>{(step) => <StepRow step={step} />}</For>
          <Show when={props.meta.session}>
            <div
              class="chat-connector-session"
              title="The harness's own conversation for this chat, continued on the next message"
            >
              session {props.meta.session}
              {props.meta.version ? ` · ${props.meta.version}` : ''}
            </div>
          </Show>
        </div>
      </Show>
    </div>
  );
}

function StepRow(props: { step: HarnessStep }) {
  const [open, setOpen] = createSignal(false);
  const status = () =>
    props.step.ok === undefined || props.step.ok === null
      ? 'running'
      : props.step.ok
        ? 'ok'
        : 'error';
  const inputText = () => {
    const input = props.step.input;
    if (input === null || input === undefined) return '';
    if (typeof input === 'string') return input;
    try {
      return JSON.stringify(input, null, 2);
    } catch {
      return String(input);
    }
  };
  return (
    <div
      class={`chat-connector-step ${status()}`}
      classList={{ sub: !!props.step.parent, open: open() }}
    >
      <button
        type="button"
        class="chat-connector-step-line"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open()}
      >
        <span class="chat-connector-step-mark" aria-hidden="true">
          {status() === 'ok' ? '✓' : status() === 'error' ? '✕' : '…'}
        </span>
        <span class="chat-connector-step-label">{stepLabel(props.step)}</span>
        <Show when={props.step.parent}>
          <span class="tool-group-meta">subagent</span>
        </Show>
      </button>
      <Show when={open()}>
        <div class="chat-connector-step-body">
          <Show when={inputText()}>
            <pre class="chat-connector-step-input">{inputText()}</pre>
          </Show>
          <Show when={props.step.result}>
            <pre class="chat-connector-step-result">{props.step.result}</pre>
          </Show>
        </div>
      </Show>
    </div>
  );
}
