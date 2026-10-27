// ConnectorSteps.tsx
//
// What a connector's harness did for a reply, running its own tools, shown
// with the chat's own parts as Rovik's agent's work is: each stretch of its
// thinking a ThinkingBlock, its tool calls the lines and blocks of Rovik's
// tools of the same kind (ToolCallGroup; the kind's ConnectorSpec says
// which), and what it wrote before each as the reply's words show, in the
// order it went (lib/connectors.ts connectorSegments). A call waiting for
// the user's approval shows the request, as Rovik's do, and one waiting on
// what Rovik installs for it says so on its line.
import { createMemo, Index, Match, Switch } from 'solid-js';
import type { ChatMessage, ChatToolCall } from '../types';
import {
  connectorSegments,
  type ConnectorMeta,
  type ConnectorSegment,
  type KeptStep,
} from '../lib/connectors';
import { groupToolCalls } from '../lib/toolGroups';
import MessageContent from './MessageContent';
import ThinkingBlock from './ThinkingBlock';
import ToolCallBlock from './ToolCallBlock';
import ToolCallGroup from './ToolCallGroup';

const thoughtOf = (s: ConnectorSegment) => ('thought' in s ? s.thought : undefined);
const callsOf = (s: ConnectorSegment) => ('calls' in s ? s.calls : undefined);
const textOf = (s: ConnectorSegment) => ('text' in s ? s.text : undefined);

export default function ConnectorSteps(props: {
  meta: ConnectorMeta;
  /** The reply's text: what it wrote before a step shows in its place. */
  text?: string;
  /** The reply is still coming: its last thought may go on, a call may run. */
  running: boolean;
  /** The chat it's in: its calls' approvals wait there. */
  dialogId: string | null;
  /** Live notes on its calls by id while they wait, e.g. on a toolchain's install. */
  notes?: Record<string, string>;
}) {
  const kept = new Map<string, KeptStep>();
  const segments = createMemo(() => connectorSegments(props.meta, kept, props.text));
  const results = createMemo(() => {
    const byCall = new Map<ChatToolCall, ChatMessage | undefined>();
    for (const segment of segments()) {
      for (const step of callsOf(segment) ?? []) byCall.set(step.call, step.result);
    }
    return byCall;
  });
  const resultOf = (call: ChatToolCall) => results().get(call);
  const renderCall = (call: ChatToolCall, expanded: boolean) => (
    <ToolCallBlock
      initiallyExpanded={expanded}
      dialogId={props.dialogId}
      toolCall={call}
      result={resultOf(call)}
      running={props.running}
      note={props.notes?.[call.id]}
    />
  );
  return (
    <Index each={segments()}>
      {(segment, i) => (
        <Switch>
          <Match when={thoughtOf(segment())}>
            {(step) => (
              <ThinkingBlock
                text={(step().thinking ?? '').trimEnd()}
                ms={step().ms}
                thinking={props.running && i === segments().length - 1 && step().ms == null}
              />
            )}
          </Match>
          <Match when={textOf(segment())}>
            {(text) => (
              <div class="chat-message assistant">
                <MessageContent content={text()} />
              </div>
            )}
          </Match>
          <Match when={callsOf(segment())}>
            {(steps) => (
              <div class="message-tools">
                <Index each={groupToolCalls(steps().map((s) => s.call))}>
                  {(run) => (
                    <ToolCallGroup
                      name={run().name}
                      calls={run().calls}
                      dialogId={props.dialogId}
                      result={resultOf}
                      running={props.running}
                      renderCall={renderCall}
                    />
                  )}
                </Index>
              </div>
            )}
          </Match>
        </Switch>
      )}
    </Index>
  );
}
