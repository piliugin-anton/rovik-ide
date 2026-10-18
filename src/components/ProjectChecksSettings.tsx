// ProjectChecksSettings.tsx
//
// Settings page for the open project's checks: what the verification gate
// runs when the agent finishes a change (src-tauri/src/agent_run/verify.rs).
// Shows what was detected and lets the user correct it; the corrected list
// is saved per project right away and wins over detection from then on.
import { createMemo, createSignal, For, onMount, Show } from 'solid-js';
import {
  CHECK_KINDS,
  CHECK_LABEL,
  detectProject,
  effectiveChecks,
  loadProjectSettings,
  saveProjectSettings,
  scriptKind,
  workspaceMemberCommand,
  workspaceScriptCommand,
  workspaceScriptSuggestions,
  type CheckKind,
  type ProjectCheck,
  type ProjectProfile,
  type WorkspaceMember,
} from '../lib/projectProfile';
import { checkCommand, type AgentPermissions } from '../lib/permissions';

export default function ProjectChecksSettings(props: {
  root: string;
  permissions: AgentPermissions;
  onPermissionsChange: (next: AgentPermissions) => void;
}) {
  const [profile, setProfile] = createSignal<ProjectProfile | null>(null);
  const [checks, setChecks] = createSignal<ProjectCheck[]>([]);
  const [customized, setCustomized] = createSignal(false);
  const [loading, setLoading] = createSignal(true);
  const [error, setError] = createSignal<string | null>(null);

  const folder = () => props.root.split(/[\\/]/).filter(Boolean).pop() ?? props.root;
  const p = () => props.permissions;

  onMount(async () => {
    try {
      const [detected, settings] = await Promise.all([
        detectProject(props.root),
        loadProjectSettings(props.root),
      ]);
      setProfile(detected);
      setChecks(effectiveChecks(detected, settings));
      setCustomized(!!settings.checks);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  });

  function save(next: ProjectCheck[]) {
    setChecks(next);
    setCustomized(true);
    saveProjectSettings(props.root, { checks: next }).catch((err) =>
      setError(`Couldn't save: ${String(err)}`)
    );
  }

  function update(index: number, patch: Partial<ProjectCheck>) {
    save(
      checks().map((c, i) => {
        if (i !== index) return c;
        const next = { ...c, ...patch };
        // An edited command is no longer the detected one: drop its origin.
        if (patch.command !== undefined && patch.command !== c.command) delete next.source;
        return next;
      })
    );
  }

  function addSuggestion(script: string) {
    const profileValue = profile();
    const command = workspaceScriptCommand(profileValue?.packageManager, script);
    if (checks().some((c) => c.command.trim() === command)) return;
    save([...checks(), { kind: scriptKind(script), command, enabled: true }]);
  }

  // Compact per-member picker: one package + one script → one scoped check.
  // Cargo members expose check kinds (covered by the workspace root otherwise);
  // node members expose their own package.json scripts.
  // NOTE: workspacesProfile is defined before the memos below — the memos
  // evaluate eagerly, so referencing it before its `const` runs throws TDZ
  // "Cannot access before initialization".
  /** The detected profile when it has workspace members (undefined while loading). */
  const workspacesProfile = (): ProjectProfile | undefined => {
    const found = profile();
    if (loading() || !found || (found.workspaces?.length ?? 0) === 0) return undefined;
    return found;
  };
  const [memberPath, setMemberPath] = createSignal<string>('');
  const [memberScript, setMemberScript] = createSignal<string>('');
  const memberOptions = createMemo(() => workspacesProfile()?.workspaces ?? []);
  const selectedMember = createMemo(
    () =>
      memberOptions().find((m) => m.path === memberPath()) ??
      memberOptions()[0] ??
      (undefined as WorkspaceMember | undefined)
  );
  const scriptOptions = createMemo(() => {
    const m = selectedMember();
    if (!m) return [] as string[];
    return m.kind === 'cargo' ? ['typecheck', 'lint', 'test', 'build'] : m.scripts;
  });
  const selectedScript = createMemo(() => {
    const opts = scriptOptions();
    if (opts.includes(memberScript())) return memberScript();
    return opts[0] ?? '';
  });

  function addMemberCheck() {
    const m = selectedMember();
    const script = selectedScript();
    if (!m || !script) return;
    const built = workspaceMemberCommand(profile()?.packageManager, m, script);
    if (checks().some((c) => c.command.trim() === built.command)) return;
    save([...checks(), { ...built, enabled: true }]);
  }

  function resetToDetected() {
    setChecks(effectiveChecks(profile(), {}));
    setCustomized(false);
    saveProjectSettings(props.root, null).catch((err) => setError(`Couldn't save: ${String(err)}`));
  }

  /** Whether the user's command permissions let this check run. */
  const allowed = (command: string) => !command.trim() || checkCommand(command, p()).permitted;

  function allow(command: string) {
    props.onPermissionsChange({
      ...p(),
      allowedCommands: [...p().allowedCommands, command.trim()],
    });
  }

  return (
    <div class="permissions-settings project-checks">
      <section class="settings-section">
        <div class="settings-section-title">Checking changes</div>
        <label class="setting-checkbox">
          <input
            type="checkbox"
            checked={p().verifyChanges}
            onChange={(e) =>
              props.onPermissionsChange({ ...p(), verifyChanges: e.currentTarget.checked })
            }
          />
          <span>
            Check the agent’s changes when it finishes
            <span class="setting-checkbox-sub">
              After a task that changed files, the checks below run. Failures go back to the agent
              to fix, up to 3 times. Checks follow your command settings: one that isn’t allowed is
              skipped, and with command approval on you start them yourself.
            </span>
          </span>
        </label>
      </section>

      <section class="settings-section">
        <div class="settings-section-title">Checks for {folder()}</div>
        <Show when={!loading()} fallback={<p class="settings-hint">Looking at the project…</p>}>
          <p class="settings-hint">
            {customized()
              ? 'Your list for this project.'
              : 'Found in the project’s files. Edit them to make the list yours.'}{' '}
            They run in this order: type check, lint, tests, build.
            <Show when={customized()}>
              {' '}
              <button class="link-button" onClick={resetToDetected}>
                Use detected checks
              </button>
            </Show>
          </p>

          <div class="project-check-list">
            <For
              each={checks()}
              fallback={
                <div class="command-list-empty">
                  No checks. Add the commands that build, lint or test this project.
                </div>
              }
            >
              {(check, index) => (
                <div class="project-check">
                  <div class="project-check-row">
                    <input
                      type="checkbox"
                      checked={check.enabled}
                      onChange={(e) => update(index(), { enabled: e.currentTarget.checked })}
                      aria-label={`Run ${CHECK_LABEL[check.kind]}`}
                      title="Run this check"
                    />
                    <select
                      class="settings-select project-check-kind"
                      value={check.kind}
                      onChange={(e) =>
                        update(index(), { kind: e.currentTarget.value as CheckKind })
                      }
                    >
                      <For each={CHECK_KINDS}>
                        {(kind) => <option value={kind}>{CHECK_LABEL[kind]}</option>}
                      </For>
                    </select>
                    <input
                      class="project-check-command"
                      value={check.command}
                      placeholder="Command, e.g. npm test"
                      spellcheck={false}
                      onChange={(e) => update(index(), { command: e.currentTarget.value })}
                    />
                    <button
                      class="icon-button"
                      onClick={() => save(checks().filter((_, i) => i !== index()))}
                      aria-label={`Remove ${check.command}`}
                      title="Remove"
                    >
                      ×
                    </button>
                  </div>
                  <Show when={check.source}>
                    <p class="settings-hint project-check-source" title={check.source}>
                      Detected: {check.source}
                    </p>
                  </Show>
                  <Show when={check.enabled && !allowed(check.command)}>
                    <p class="settings-hint project-check-warning">
                      Not in your allowed commands, so it’s skipped.{' '}
                      <button class="link-button" onClick={() => allow(check.command)}>
                        Allow it
                      </button>
                    </p>
                  </Show>
                </div>
              )}
            </For>
          </div>
          <button
            class="link-button project-check-add"
            onClick={() => save([...checks(), { kind: 'test', command: '', enabled: true }])}
          >
            Add a check
          </button>
        </Show>
        <Show when={error()}>
          <p class="settings-hint setting-explain warning">{error()}</p>
        </Show>
      </section>

      <Show when={workspacesProfile()}>
        {(found) => (
          <details class="settings-section workspace-details">
            <summary class="workspace-summary">
              <span class="settings-section-title">
                Workspaces ({found().workspaces!.length}{' '}
                {found().workspaces!.length === 1 ? 'package' : 'packages'})
              </span>
              <span class="settings-hint">Add a check for one package</span>
            </summary>
            <div class="workspace-add-row">
              <select
                class="settings-select workspace-select"
                value={selectedMember()?.path ?? ''}
                onChange={(e) => {
                  setMemberPath(e.currentTarget.value);
                  setMemberScript('');
                }}
                aria-label="Package"
                title="Package"
              >
                <For each={memberOptions()}>
                  {(m) => (
                    <option value={m.path}>{m.name ? `${m.name} (${m.path})` : m.path}</option>
                  )}
                </For>
              </select>
              <select
                class="settings-select workspace-select"
                value={selectedScript()}
                onChange={(e) => setMemberScript(e.currentTarget.value)}
                aria-label="Script"
                title="Script"
              >
                <For each={scriptOptions()}>{(s) => <option value={s}>{s}</option>}</For>
              </select>
              <button
                class="link-button"
                onClick={addMemberCheck}
                disabled={!selectedMember() || !selectedScript()}
                title={
                  selectedMember() && selectedScript()
                    ? workspaceMemberCommand(
                        found().packageManager,
                        selectedMember()!,
                        selectedScript()
                      ).command
                    : undefined
                }
              >
                Add
              </button>
            </div>
            <Show when={workspaceScriptSuggestions(found(), checks()).length > 0}>
              <div class="workspace-suggestions">
                <span class="settings-hint">All packages:</span>
                <For each={workspaceScriptSuggestions(found(), checks())}>
                  {(suggestion) => (
                    <button
                      class="workspace-suggestion"
                      onClick={() => addSuggestion(suggestion.script)}
                      title={`${suggestion.command} — runs in ${suggestion.packages} ${suggestion.packages === 1 ? 'package' : 'packages'}`}
                    >
                      {suggestion.script}
                      <span class="workspace-suggestion-count">×{suggestion.packages}</span>
                    </button>
                  )}
                </For>
              </div>
            </Show>
          </details>
        )}
      </Show>

      <Show when={profile()}>
        {(found) => (
          <section class="settings-section">
            <div class="settings-section-title">What the agent is told</div>
            <p class="settings-hint">
              {found().stack.length ? `Stack: ${found().stack.join(', ')}. ` : ''}
              {found().instructions
                ? `Instructions for agents: ${found().instructions!.file}${found().instructions!.truncated ? ' (shortened)' : ''}.`
                : 'No instructions found. Add an AGENTS.md to tell the agent how to work in this project.'}
            </p>
          </section>
        )}
      </Show>
    </div>
  );
}
