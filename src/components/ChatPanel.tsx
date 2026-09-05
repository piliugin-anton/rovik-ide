import { createSignal, onMount, onCleanup, For, Show, createMemo, createEffect } from 'solid-js';
import ModelSettingsPanel from './ModelSettingsPanel';
import { loadAiSettings, saveAiSettings } from '../lib/aiSettings';
import { sendChatMessage } from '../lib/aiChat';
import { AGENT_TOOL_SCHEMAS, executeAgentTool } from '../lib/tools';
import { buildContextHeader } from '../lib/activeContext';
import type { ActiveFileContext } from '../lib/activeContext';
import type { ChatMessage, ChatStreamEvent, ChatToolCall, ModelProfile } from '../types';
import { Channel } from '@tauri-apps/api/core';

// Hard ceiling on how many read→act round-trips one user message can trigger
// before we stop and hand control back — a model that keeps calling tools
// without ever producing a final answer would otherwise loop forever.
const MAX_AGENT_STEPS = 8;

export default function ChatPanel(props: {
  rootPath: () => string | null;
  activeFileContext: () => ActiveFileContext | null;
}) {
  const [profiles, setProfiles] = createSignal<ModelProfile[]>([]);
  const [activeProfileId, setActiveProfileId] = createSignal<string | null>(null);
  const [messages, setMessages] = createSignal<ChatMessage[]>([]);
  const [input, setInput] = createSignal('');
  const [settingsOpen, setSettingsOpen] = createSignal(false);
  const [sending, setSending] = createSignal(false);

  // Track current in-flight request for cleanup
  let currentChannel: Channel<ChatStreamEvent> | null = null;
  let rafId: number | null = null;
  let isMounted = true;
  let messagesRef: HTMLDivElement | undefined;

  const activeProfile = createMemo(
    () => profiles().find((p) => p.id === activeProfileId()) ?? null
  );

  onMount(async () => {
    const settings = await loadAiSettings();
    setProfiles(settings.profiles);
    setActiveProfileId(settings.activeProfileId ?? settings.profiles[0]?.id ?? null);
  });

  createEffect(() => {
    // Track whatever should trigger a scroll
    messages();
    // Run after Solid has patched the DOM
    queueMicrotask(() => {
      if (messagesRef) {
        messagesRef.scrollTop = messagesRef.scrollHeight;
      }
    });
  });

  // Cleanup on unmount: abort any in-flight request
  onCleanup(() => {
    isMounted = false;
    if (currentChannel) {
      // Closing the channel tells Rust to stop sending events
      currentChannel = null;
    }
    if (rafId !== null) {
      cancelAnimationFrame(rafId);
      rafId = null;
    }
  });

  function persist(nextProfiles: ModelProfile[], nextActiveId: string | null) {
    void saveAiSettings({ profiles: nextProfiles, activeProfileId: nextActiveId });
  }

  function handleProfilesSaved(nextProfiles: ModelProfile[]) {
    setProfiles(nextProfiles);
    let nextActive = activeProfileId();
    if (!nextProfiles.some((p) => p.id === nextActive)) {
      nextActive = nextProfiles[0]?.id ?? null;
      setActiveProfileId(nextActive);
    }
    persist(nextProfiles, nextActive);
  }

  function handleModelChange(id: string) {
    setActiveProfileId(id);
    persist(profiles(), id);
  }

  /**
   * Runs one model turn against `history`, streams it into a pending
   * message, and — if the model asked for tool calls and a project is open
   * — executes them and recurses with the results appended, up to
   * MAX_AGENT_STEPS. Returns once the model produces a turn with no tool
   * calls, an error occurs, or the step budget runs out.
   */
  async function runAgentTurn(messages: ChatMessage[], step: number): Promise<void> {
    const profile = activeProfile();
    if (!profile) return;

    const pendingMsg: ChatMessage = {
      id: crypto.randomUUID(),
      role: 'assistant',
      content: '',
      pending: true,
      toolCalls: [],
    };
    setMessages((prev) => [...prev, pendingMsg]);

    const channel = new Channel<ChatStreamEvent>();
    currentChannel = channel;

    let assistantText = '';
    const toolCallsMap = new Map<string, ChatToolCall>();
    let pendingRender = false;
    let streamError: string | null = null;

    const flushAssistantText = () => {
      pendingRender = false;
      if (!isMounted) return;
      setMessages((prev) =>
        prev.map((m) => (m.id === pendingMsg.id ? { ...m, content: assistantText } : m))
      );
    };

    channel.onmessage = (event) => {
      // Ignore events if component unmounted
      if (!isMounted) return;

      switch (event.type) {
        case 'Delta': {
          assistantText += event.content;
          if (!pendingRender) {
            pendingRender = true;
            rafId = requestAnimationFrame(() => {
              rafId = null;
              flushAssistantText();
            });
          }
          break;
        }

        case 'Done': {
          setMessages((prev) =>
            prev.map((m) => (m.id === pendingMsg.id ? { ...m, pending: false } : m))
          );
          break;
        }

        case 'Error': {
          streamError = event.message;
          setMessages((prev) =>
            prev.map((m) =>
              m.id === pendingMsg.id ? { ...m, pending: false, error: event.message } : m
            )
          );
          break;
        }

        case 'ToolCallStart': {
          toolCallsMap.set(event.id, { id: event.id, name: event.name, arguments: '' });
          break;
        }

        case 'ToolCallArgumentsDelta': {
          const tc = toolCallsMap.get(event.id);
          if (tc) {
            tc.arguments += event.arguments_delta;
            setMessages((prev) =>
              prev.map((m) =>
                m.id === pendingMsg.id ? { ...m, toolCalls: Array.from(toolCallsMap.values()) } : m
              )
            );
          }
          break;
        }

        case 'ToolCallComplete': {
          toolCallsMap.set(event.id, {
            id: event.id,
            name: event.name,
            arguments: event.arguments,
          });
          setMessages((prev) =>
            prev.map((m) =>
              m.id === pendingMsg.id ? { ...m, toolCalls: Array.from(toolCallsMap.values()) } : m
            )
          );
          break;
        }
      }
    };

    // Tool schemas are only sent once a project folder is open — without a
    // root there's nowhere for read_file/write_file/etc. to operate.
    const root = props.rootPath();
    const tools = root ? (AGENT_TOOL_SCHEMAS as unknown as unknown[]) : undefined;

    try {
      await sendChatMessage({ profile, messages, channel, tools });
    } catch (err) {
      if (isMounted) {
        setMessages((prev) =>
          prev.map((m) =>
            m.id === pendingMsg.id ? { ...m, pending: false, error: String(err) } : m
          )
        );
      }
      return;
    } finally {
      if (currentChannel === channel) {
        currentChannel = null;
      }
    }

    if (streamError) return;

    const toolCalls = Array.from(toolCallsMap.values());

    // Nothing to act on (either the model gave a plain answer, or there's no
    // project open to run tools against) — this turn is the final answer.
    if (toolCalls.length === 0 || !root) {
      if (isMounted) {
        setMessages((prev) =>
          prev.map((m) => (m.id === pendingMsg.id ? { ...m, pending: false } : m))
        );
      }
      return;
    }

    if (step >= MAX_AGENT_STEPS) {
      if (isMounted) {
        setMessages((prev) => [
          ...prev.map((m) => (m.id === pendingMsg.id ? { ...m, pending: false } : m)),
          {
            id: crypto.randomUUID(),
            role: 'assistant',
            content: '',
            error: `Stopped after ${MAX_AGENT_STEPS} tool-call steps to avoid a runaway loop.`,
          },
        ]);
      }
      return;
    }

    // Lock in the assistant turn that requested these calls, then execute
    // each one and feed the results back so the model can continue.
    const assistantMsg: ChatMessage = {
      ...pendingMsg,
      content: assistantText,
      toolCalls,
      pending: false,
    };
    if (isMounted) {
      setMessages((prev) => prev.map((m) => (m.id === pendingMsg.id ? assistantMsg : m)));
    }

    const toolResultMsgs: ChatMessage[] = [];
    for (const tc of toolCalls) {
      const result = await executeAgentTool(root, tc.name, tc.arguments);
      const toolMsg: ChatMessage = {
        id: crypto.randomUUID(),
        role: 'tool',
        content: result,
        tool_call_id: tc.id,
      };
      toolResultMsgs.push(toolMsg);
      if (isMounted) setMessages((prev) => [...prev, toolMsg]);
    }

    await runAgentTurn([...messages, assistantMsg, ...toolResultMsgs], step + 1);
  }

  async function handleSend() {
    const text = input().trim();
    const profile = activeProfile();
    if (!text || !profile || sending()) return;

    const userMsg: ChatMessage = {
      id: crypto.randomUUID(),
      role: 'user',
      content: text,
    };

    // Displayed history stays clean; only the copy sent to the model gets
    // the lightweight editor-context breadcrumb prepended.
    const contextHeader = buildContextHeader(props.activeFileContext());
    const wireUserMsg: ChatMessage = contextHeader
      ? { ...userMsg, content: `${contextHeader}\n\n${text}` }
      : userMsg;

    const historyBase = messages();
    setMessages((prev) => [...prev, userMsg]);
    setInput('');
    setSending(true);

    try {
      await runAgentTurn([...historyBase, wireUserMsg], 0);
    } finally {
      if (isMounted) setSending(false);
    }
  }

  function handleKeyDown(e: KeyboardEvent) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      void handleSend();
    }
  }

  const title = createMemo(() =>
    activeProfile() ? `AI Chat — ${activeProfile()!.label}` : 'AI Chat'
  );

  return (
    <div class="chat-panel">
      <div class="chat-header">
        <span class="chat-title" title={title()}>
          {title()}
        </span>
        <button class="icon-button" onClick={() => setSettingsOpen(true)} title="Model settings">
          ⚙
        </button>
      </div>

      <Show when={!props.rootPath()}>
        <div class="chat-tools-hint">Open a folder to let the model read and edit your files.</div>
      </Show>

      <div ref={messagesRef} class="chat-messages">
        <For each={messages()} fallback={<div class="empty-state">Ask the model anything</div>}>
          {(m) => (
            <Show
              when={m.role !== 'tool'}
              fallback={
                <div class="chat-message tool">
                  <div class="tool-name">Tool result</div>
                  <pre class="tool-args">{m.content}</pre>
                </div>
              }
            >
              <div
                class="chat-message"
                classList={{ user: m.role === 'user', assistant: m.role === 'assistant' }}
              >
                <Show
                  when={m.content.length > 0 || !m.pending}
                  fallback={<span class="chat-pending">Thinking…</span>}
                >
                  <Show when={!m.error} fallback={<span class="chat-error">{m.error}</span>}>
                    <div class="message-content">{m.content}</div>
                    <Show when={m.toolCalls && m.toolCalls.length > 0}>
                      <div class="message-tools">
                        <For each={m.toolCalls!}>
                          {(tc) => (
                            <div class="tool-call">
                              <div class="tool-name">Tool: {tc.name}</div>
                              <pre class="tool-args">
                                {(() => {
                                  try {
                                    return JSON.stringify(JSON.parse(tc.arguments), null, 2);
                                  } catch {
                                    return tc.arguments;
                                  }
                                })()}
                              </pre>
                            </div>
                          )}
                        </For>
                      </div>
                    </Show>
                  </Show>
                </Show>
              </div>
            </Show>
          )}
        </For>
      </div>

      <div class="chat-input-row">
        <textarea
          value={input()}
          onInput={(e) => setInput(e.currentTarget.value)}
          onKeyDown={handleKeyDown}
          placeholder={activeProfile() ? 'Message the model…' : 'Add a model in settings first'}
          disabled={!activeProfile()}
        />
        <button onClick={handleSend} disabled={!activeProfile() || sending() || !input().trim()}>
          Send
        </button>
      </div>

      <div class="chat-model-select">
        <select
          value={activeProfileId() ?? ''}
          onChange={(e) => handleModelChange(e.currentTarget.value)}
          disabled={profiles().length === 0}
        >
          <Show when={profiles().length > 0} fallback={<option value="">No models saved</option>}>
            <For each={profiles()}>{(p) => <option value={p.id}>{p.label}</option>}</For>
          </Show>
        </select>
      </div>

      <Show when={settingsOpen()}>
        <ModelSettingsPanel
          profiles={profiles()}
          onSave={handleProfilesSaved}
          onClose={() => setSettingsOpen(false)}
        />
      </Show>
    </div>
  );
}
