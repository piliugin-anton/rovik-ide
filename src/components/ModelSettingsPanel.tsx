import { createSignal, For, Show } from 'solid-js';
import type { ModelProfile } from '../types';

interface ModelSettingsPanelProps {
  profiles: ModelProfile[];
  onSave: (profiles: ModelProfile[]) => void;
  onClose: () => void;
}

const emptyDraft = (): ModelProfile => ({
  id: crypto.randomUUID(),
  label: '',
  model: '',
  baseUrl: '',
  apiKey: '',
  stream: false,
});

export default function ModelSettingsPanel(props: ModelSettingsPanelProps) {
  const [profiles, setProfiles] = createSignal<ModelProfile[]>(props.profiles);
  const [draft, setDraft] = createSignal<ModelProfile | null>(null);

  function startAdd() {
    setDraft(emptyDraft());
  }

  function startEdit(p: ModelProfile) {
    setDraft({ ...p });
  }

  function removeProfile(id: string) {
    setProfiles((prev) => prev.filter((p) => p.id !== id));
  }

  function saveDraft() {
    const d = draft();
    if (!d || !d.label.trim() || !d.model.trim() || !d.baseUrl.trim()) return;
    setProfiles((prev) => {
      const exists = prev.some((p) => p.id === d.id);
      return exists ? prev.map((p) => (p.id === d.id ? d : p)) : [...prev, d];
    });
    setDraft(null);
  }

  function commitAndClose() {
    props.onSave(profiles());
    props.onClose();
  }

  return (
    <div class="modal-overlay" onClick={commitAndClose}>
      <div class="modal" onClick={(e) => e.stopPropagation()}>
        <div class="modal-header">
          <span>Model Settings</span>
          <button class="icon-button" onClick={commitAndClose}>
            ×
          </button>
        </div>

        <div class="modal-body">
          <Show
            when={!draft()}
            fallback={
              <div class="profile-form">
                <label>
                  Name
                  <input
                    value={draft()!.label}
                    onInput={(e) => setDraft({ ...draft()!, label: e.currentTarget.value })}
                    placeholder="e.g. GPT-4o mini"
                  />
                </label>
                <label>
                  Model
                  <input
                    value={draft()!.model}
                    onInput={(e) => setDraft({ ...draft()!, model: e.currentTarget.value })}
                    placeholder="e.g. gpt-4o-mini"
                  />
                </label>
                <label>
                  Base URL
                  <input
                    value={draft()!.baseUrl}
                    onInput={(e) => setDraft({ ...draft()!, baseUrl: e.currentTarget.value })}
                    placeholder="https://api.openai.com/v1"
                  />
                </label>
                <label>
                  API Key
                  <input
                    type="password"
                    value={draft()!.apiKey}
                    onInput={(e) => setDraft({ ...draft()!, apiKey: e.currentTarget.value })}
                    placeholder="sk-…"
                  />
                </label>
                <label>
                  Streaming?
                  <input
                    type="checkbox"
                    checked={draft()!.stream}
                    onChange={(e) => setDraft({ ...draft()!, stream: e.currentTarget.checked })}
                  />
                </label>
                <div class="profile-form-actions">
                  <button onClick={() => setDraft(null)}>Cancel</button>
                  <button class="primary" onClick={saveDraft}>
                    Save
                  </button>
                </div>
              </div>
            }
          >
            <For each={profiles()} fallback={<div class="empty-state">No saved models yet</div>}>
              {(p) => (
                <div class="profile-row">
                  <div class="profile-row-info">
                    <span class="profile-row-label">{p.label}</span>
                    <span class="profile-row-sub">
                      {p.model} · {p.baseUrl}
                    </span>
                  </div>
                  <div class="profile-row-actions">
                    <button onClick={() => startEdit(p)}>Edit</button>
                    <button onClick={() => removeProfile(p.id)}>Delete</button>
                  </div>
                </div>
              )}
            </For>
            <button class="add-profile-button" onClick={startAdd}>
              + Add model
            </button>
          </Show>
        </div>
      </div>
    </div>
  );
}
