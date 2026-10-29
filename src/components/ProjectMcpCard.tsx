// ProjectMcpCard.tsx
//
// The project's own MCP servers that wait for the user (projectMcpServers.ts,
// mcp-plan-2.md Q12), above the chat's input as the agent's questions are:
// each server with what it runs and its file, a field for each variable its
// values name (filled from this computer's environment where it has one;
// one whose value is saved isn't asked for again), and what the project's
// files have that isn't used. Allow keeps the values and the servers for
// the next run; Not now puts them off until Rovik is closed. One that asks
// for a sign-in is signed in to from the settings page once allowed (Q14).
import { createSignal, For, onMount, Show } from 'solid-js';
import { serverSummary, signsIn } from '../lib/mcpServers';
import {
  allowServers,
  prefillValues,
  settleAwaitingServers,
  variablesOf,
  type AwaitingServers,
  type ProjectVariable,
} from '../lib/projectMcpServers';

export default function ProjectMcpCard(props: {
  dialogId: string;
  awaiting: AwaitingServers;
  /** The project's folder on this computer; null when it isn't the open one. */
  root: string | null;
}) {
  const servers = () => props.awaiting.servers;
  const leftOut = () => props.awaiting.leftOut ?? [];
  const variables = () => variablesOf(servers());
  const [values, setValues] = createSignal<Record<string, string>>({});
  const [saved, setSaved] = createSignal<string[]>([]);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  onMount(async () => {
    const names = variables().map((v) => v.name);
    if (names.length === 0) return;
    try {
      const found = await prefillValues(props.awaiting.projectId, names);
      setValues((typed) => ({ ...found.values, ...typed }));
      setSaved(found.saved);
    } catch (e) {
      console.error("The variables' values weren't read:", e);
    }
  });

  /** The variables with no value, saved or typed, and no default: Allow waits for them. */
  const missing = () =>
    variables().filter((v) => !v.default && !saved().includes(v.name) && !values()[v.name]?.trim());
  const placeholder = (v: ProjectVariable) =>
    saved().includes(v.name)
      ? 'Saved: type to replace'
      : v.default
        ? `${v.default} unless given`
        : 'Its value';

  async function allow() {
    const root = props.root;
    if (!root) return;
    setBusy(true);
    setError(null);
    try {
      await allowServers(root, props.awaiting.projectId, servers(), values());
      settleAwaitingServers(props.dialogId, false);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="question-panel mcp-awaiting" role="dialog" aria-label="The project's MCP servers">
      <div class="question-panel-head">
        <span class="question-panel-step">
          {servers().length > 0
            ? "This project's files set up MCP servers"
            : "Some of this project's MCP servers aren't used"}
        </span>
      </div>
      <Show when={servers().length > 0}>
        <div class="mcp-awaiting-why">
          The agent can use their tools once you allow them. Allow only servers you trust: they run
          as you, outside Rovik's protections. A server whose file changes asks again.
          <Show when={servers().some(signsIn)}>
            {' '}
            One that asks you to sign in is signed in to from Settings → MCP servers, once allowed.
          </Show>
        </div>
        <div class="mcp-awaiting-servers">
          <For each={servers()}>
            {(server) => (
              <div class="mcp-awaiting-server">
                <span class="mcp-awaiting-name">{server.name}</span>
                <span class="mcp-awaiting-what">{serverSummary(server)}</span>
                <span class="mcp-awaiting-file">{server.file}</span>
              </div>
            )}
          </For>
        </div>
        <For each={variables()}>
          {(variable) => (
            <label class="mcp-awaiting-variable">
              <span>
                {variable.name}
                <Show when={variable.description}>{(d) => <> · {d()}</>}</Show>
              </span>
              <input
                type="password"
                aria-label={variable.name}
                placeholder={placeholder(variable)}
                value={values()[variable.name] ?? ''}
                onInput={(e) => {
                  const value = e.currentTarget.value;
                  setValues((all) => ({ ...all, [variable.name]: value }));
                }}
              />
            </label>
          )}
        </For>
      </Show>
      <Show when={leftOut().length > 0}>
        <ul class="mcp-awaiting-left-out">
          <For each={leftOut()}>{(why) => <li>{why}</li>}</For>
        </ul>
      </Show>
      <Show when={error()}>{(e) => <div class="settings-status error">{e()}</div>}</Show>
      <Show when={servers().length > 0 && !props.root}>
        <div class="settings-status">Open the project to allow its servers.</div>
      </Show>
      <div class="mcp-awaiting-actions">
        <Show when={servers().length > 0}>
          <button
            type="button"
            class="primary"
            disabled={busy() || !props.root || missing().length > 0}
            title={missing().length > 0 ? `Give ${missing()[0].name} a value first` : undefined}
            onClick={() => void allow()}
          >
            Allow
          </button>
        </Show>
        <button type="button" onClick={() => settleAwaitingServers(props.dialogId, true)}>
          {servers().length > 0 ? 'Not now' : 'OK'}
        </button>
      </div>
    </div>
  );
}
