import { createSignal, onMount, onCleanup, For, Show, createMemo } from "solid-js";
import ModelSettingsPanel from "./ModelSettingsPanel";
import { loadAiSettings, saveAiSettings } from "../lib/aiSettings";
import { sendChatMessage } from "../lib/aiChat";
import type { ChatMessage, ChatStreamEvent, ChatToolCall, ModelProfile } from "../types";
import { Channel } from "@tauri-apps/api/core";

export default function ChatPanel() {
  const [profiles, setProfiles] = createSignal<ModelProfile[]>([]);
  const [activeProfileId, setActiveProfileId] = createSignal<string | null>(null);
  const [messages, setMessages] = createSignal<ChatMessage[]>([]);
  const [input, setInput] = createSignal("");
  const [settingsOpen, setSettingsOpen] = createSignal(false);
  const [sending, setSending] = createSignal(false);

  // Track current in-flight request for cleanup
  let currentChannel: Channel<ChatStreamEvent> | null = null;
  let isMounted = true;

  const activeProfile = createMemo(() =>
    profiles().find((p) => p.id === activeProfileId()) ?? null
  );

  onMount(async () => {
    const settings = await loadAiSettings();
    setProfiles(settings.profiles);
    setActiveProfileId(settings.activeProfileId ?? settings.profiles[0]?.id ?? null);
  });

  // Cleanup on unmount: abort any in-flight request
  onCleanup(() => {
    isMounted = false;
    if (currentChannel) {
      // Closing the channel tells Rust to stop sending events
      currentChannel = null;
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

  async function handleSend() {
    const text = input().trim();
    const profile = activeProfile();
    if (!text || !profile || sending()) return;

    const userMsg: ChatMessage = {
      id: crypto.randomUUID(),
      role: "user",
      content: text,
    };

    const pendingMsg: ChatMessage = {
      id: crypto.randomUUID(),
      role: "assistant",
      content: "",
      pending: true,
      toolCalls: [],
    };

    setMessages((prev) => [...prev, userMsg, pendingMsg]);
    setInput("");
    setSending(true);

    // Create a NEW channel for THIS request only
    const channel = new Channel<ChatStreamEvent>();
    currentChannel = channel;

    try {
      // Capture history at moment of send
      const history = [...messages(), userMsg];

      let assistantText = "";
      const toolCallsMap = new Map<string, ChatToolCall>();

      let pendingRender = false;

const flushAssistantText = () => {
  pendingRender = false;

  if (!isMounted) return;

  setMessages((prev) =>
    prev.map((m) =>
      m.id === pendingMsg.id
        ? { ...m, content: assistantText }
        : m
    )
  );
};

      channel.onmessage = (event) => {
        // Ignore events if component unmounted
        if (!isMounted) return;

        switch (event.type) {
          case "Delta": {
            assistantText += event.content;
            if (!pendingRender) {
        pendingRender = true;

        requestAnimationFrame(() => {
          flushAssistantText();
        });
      }
            break;
          }

          case "Done": {
            setMessages((prev) =>
              prev.map((m) =>
                m.id === pendingMsg.id ? { ...m, pending: false } : m
              )
            );
            break;
          }

          case "Error": {
            setMessages((prev) =>
              prev.map((m) =>
                m.id === pendingMsg.id
                  ? { ...m, pending: false, error: event.message }
                  : m
              )
            );
            break;
          }

          case "ToolCallStart": {
            toolCallsMap.set(event.id, {
              id: event.id,
              name: event.name,
              arguments: "",
            });
            break;
          }

          case "ToolCallArgumentsDelta": {
            const tc = toolCallsMap.get(event.id);
            if (tc) {
              tc.arguments += event.arguments_delta;
              setMessages((prev) =>
                prev.map((m) =>
                  m.id === pendingMsg.id
                    ? { ...m, toolCalls: Array.from(toolCallsMap.values()) }
                    : m
                )
              );
            }
            break;
          }

          case "ToolCallComplete": {
            toolCallsMap.set(event.id, {
              id: event.id,
              name: event.name,
              arguments: event.arguments,
            });
            setMessages((prev) =>
              prev.map((m) =>
                m.id === pendingMsg.id
                  ? { ...m, toolCalls: Array.from(toolCallsMap.values()) }
                  : m
              )
            );
            break;
          }
        }
      };

      await sendChatMessage(profile, history, channel);

      // Safety finalize (if stream ends without explicit Done)
      if (isMounted) {
        setMessages((prev) =>
          prev.map((m) =>
            m.id === pendingMsg.id
              ? {
                  ...m,
                  toolCalls: Array.from(toolCallsMap.values()),
                  pending: false,
                }
              : m
          )
        );
      }
    } catch (err) {
      if (isMounted) {
        setMessages((prev) =>
          prev.map((m) =>
            m.id === pendingMsg.id
              ? { ...m, pending: false, error: String(err) }
              : m
          )
        );
      }
    } finally {
      // Clean up channel reference
      if (currentChannel === channel) {
        currentChannel = null;
      }
      if (isMounted) setSending(false);
    }
  }

  function handleKeyDown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void handleSend();
    }
  }

  const title = createMemo(() =>
    activeProfile() ? `AI Chat — ${activeProfile()!.label}` : "AI Chat"
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

      <div class="chat-messages">
        <For
          each={messages()}
          fallback={<div class="empty-state">Ask the model anything</div>}
        >
          {(m) => (
            <div
              class="chat-message"
              classList={{ user: m.role === "user", assistant: m.role === "assistant" }}
            >
              <Show
                when={!m.pending}
                fallback={<span class="chat-pending">…</span>}
              >
                <Show
                  when={!m.error}
                  fallback={<span class="chat-error">{m.error}</span>}
                >
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
                                  return JSON.stringify(
                                    JSON.parse(tc.arguments),
                                    null,
                                    2
                                  );
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
          )}
        </For>
      </div>

      <div class="chat-input-row">
        <textarea
          value={input()}
          onInput={(e) => setInput(e.currentTarget.value)}
          onKeyDown={handleKeyDown}
          placeholder={
            activeProfile() ? "Message the model…" : "Add a model in settings first"
          }
          disabled={!activeProfile()}
        />
        <button
          onClick={handleSend}
          disabled={!activeProfile() || sending() || !input().trim()}
        >
          Send
        </button>
      </div>

      <div class="chat-model-select">
        <select
          value={activeProfileId() ?? ""}
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