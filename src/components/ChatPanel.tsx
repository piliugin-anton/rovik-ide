import { createSignal, onMount, For, Show, createMemo } from "solid-js";
import ModelSettingsPanel from "./ModelSettingsPanel";
import { loadAiSettings, saveAiSettings } from "../lib/aiSettings";
import { sendChatMessage } from "../lib/aiChat";
import type { ChatMessage, ChatStreamEvent, ModelProfile } from "../types";
import { Channel } from "@tauri-apps/api/core";

export default function ChatPanel() {
  const [profiles, setProfiles] = createSignal<ModelProfile[]>([]);
  const [activeProfileId, setActiveProfileId] = createSignal<string | null>(null);
  const [messages, setMessages] = createSignal<ChatMessage[]>([]);
  const [input, setInput] = createSignal("");
  const [settingsOpen, setSettingsOpen] = createSignal(false);
  const [sending, setSending] = createSignal(false);
  const channel = new Channel<ChatStreamEvent>()

  const activeProfile = createMemo(() => profiles().find((p) => p.id === activeProfileId()) ?? null);

  onMount(async () => {
    const settings = await loadAiSettings();
    setProfiles(settings.profiles);
    setActiveProfileId(settings.activeProfileId ?? settings.profiles[0]?.id ?? null);
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

    const userMsg: ChatMessage = { id: crypto.randomUUID(), role: "user", content: text };
    const pendingMsg: ChatMessage = { id: crypto.randomUUID(), role: "assistant", content: "", pending: true };
    setMessages((prev) => [...prev, userMsg, pendingMsg]);
    setInput("");
    setSending(true);

    try {
      const history = [...messages(), userMsg];
      let assistantText = "";
      channel.onmessage = (event) => {
    if (event.type === "Delta") {
      assistantText += event.data;

      // Update your Solid signal/store here.
      console.log(assistantText);
    }

    if (event.type === "Done") {
      console.log("stream finished");
    }

    if (event.type === "Error") {
      console.error(event.data);
    }
  };
      const result = await sendChatMessage(profile, history, channel);
      
      /* setMessages((prev) =>
        prev.map((m) =>
          m.id === pendingMsg.id ? { ...m, content: result.content, toolCalls: result.toolCalls, pending: false } : m
        )
      ); */
    } catch (err) {
      setMessages((prev) =>
        prev.map((m) => (m.id === pendingMsg.id ? { ...m, pending: false, error: String(err) } : m))
      );
    } finally {
      setSending(false);
    }
  }

  function handleKeyDown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void handleSend();
    }
  }

  const title = createMemo(() => (activeProfile() ? `AI Chat — ${activeProfile()!.label}` : "AI Chat"));

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
        <For each={messages()} fallback={<div class="empty-state">Ask the model anything</div>}>
          {(m) => (
            <div class="chat-message" classList={{ user: m.role === "user", assistant: m.role === "assistant" }}>
              <Show when={!m.pending} fallback={<span class="chat-pending">…</span>}>
                <Show when={!m.error} fallback={<span class="chat-error">{m.error}</span>}>
                  {m.content}
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
          placeholder={activeProfile() ? "Message the model…" : "Add a model in settings first"}
          disabled={!activeProfile()}
        />
        <button onClick={handleSend} disabled={!activeProfile() || sending() || !input().trim()}>
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