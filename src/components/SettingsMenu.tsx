// SettingsMenu.tsx
//
// The Settings dialog's menu: its pages in groups (the editor, the AI
// chats, the open project, this computer), each with what it's for and
// the state it's in now, so what a page affects is clear before it's
// opened. The AI pages' state comes from the settings store, the editor's
// from its store; the project's and this computer's from the backend, read
// when the menu opens (best effort: a state that can't be read is left
// out).
import { createResource, Show, type JSX } from 'solid-js';
import type { SettingsPage } from '../lib/appSettings';
import {
  aiAutopilot,
  aiMcpServers,
  aiPermissions,
  aiProviders,
  aiSettingsLoaded,
  aiSubagents,
} from '../lib/aiSettingsStore';
import { editorSettings, editorSummary } from '../lib/editorSettings';
import { providersSummary } from '../lib/aiSettings';
import { cantRun, connectorAuthState, connectorsSummary, splitSources } from '../lib/connectors';
import { agentRunsSummary, permissionsSummary } from '../lib/permissions';
import { mcpServersSummary, unsavedSecrets } from '../lib/mcpServers';
import { subagentSummary } from '../lib/subagentSettings';
import { autopilotSummary } from '../lib/autopilotSettings';
import { workloadStatus, workloadSummary } from '../lib/workload';
import { vfsStatus, vfsSummary } from '../lib/virtualFiles';
import { hostsSummary, listHosts } from '../lib/hosts';
import { networkSettings, networkSummary } from '../lib/network';
import { onWindows, wslStatus, wslSummary } from '../lib/wsl';
import { checkingForUpdates, updateInfo, updatesSummary, updateStatus } from '../lib/updates';
import { countChecksThatRun, projectChecksSummary } from '../lib/projectProfile';
import { loadRestrictedFiles, restrictedSummary } from '../lib/restrictedFiles';

interface SettingsMenuProps {
  /** The open project, for its group; none without a project. */
  rootPath: string | null;
  onOpen: (page: SettingsPage) => void;
}

/** The name of the open project's folder. */
export function folderName(root: string): string {
  return root.split(/[\\/]/).filter(Boolean).pop() ?? root;
}

export default function SettingsMenu(props: SettingsMenuProps) {
  // This computer's state, read once per opening of the menu.
  const [workload] = createResource(() => workloadStatus().catch(() => null));
  const [vfs] = createResource(() => vfsStatus().catch(() => null));
  const [hosts] = createResource(() => listHosts().catch(() => null));
  const [network] = createResource(() => networkSettings().catch(() => null));
  // WSL is this computer's Linux side on Windows alone.
  const [wsl] = createResource(() => (onWindows() ? wslStatus().catch(() => null) : null));
  // How many of the open project's checks run: none until the user ticks one.
  const [checksRunning] = createResource(
    () => props.rootPath,
    (root) => countChecksThatRun(root).catch(() => null)
  );
  // The files kept from the agent there.
  const [restricted] = createResource(
    () => props.rootPath,
    (root) => loadRestrictedFiles(root).catch(() => null)
  );
  const vfsLine = () => {
    const status = vfs();
    return status ? vfsSummary(status) : null;
  };
  const wslLine = () => {
    const status = wsl();
    return status ? wslSummary(status) : null;
  };
  /** An AI page's state line, once the settings are read. */
  const ai = (line: () => string) => (aiSettingsLoaded() ? line() : undefined);

  const Item = (item: {
    page: SettingsPage;
    label: string;
    /** What the page is for: what its settings affect. */
    about: string;
    /** The state it's in now; none while it isn't known. */
    state?: string | null;
    /** The state wants attention (nothing set up, a feature that can't work). */
    warning?: boolean;
  }) => (
    <button class="settings-menu-item" onClick={() => props.onOpen(item.page)}>
      <span class="settings-menu-text">
        <span class="settings-menu-label">{item.label}</span>
        <span class="settings-menu-sub">{item.about}</span>
        <Show when={item.state}>
          <span class="settings-menu-state" classList={{ warning: item.warning }}>
            {item.state}
          </span>
        </Show>
      </span>
      <span class="settings-menu-chevron" aria-hidden="true">
        ›
      </span>
    </button>
  );

  const Group = (group: { title: string; children: JSX.Element }) => (
    <section class="settings-menu-group" aria-label={group.title}>
      <div class="settings-menu-group-title">{group.title}</div>
      {group.children}
    </section>
  );

  return (
    <div class="settings-menu">
      <Group title="Editor">
        <Item
          page="editor"
          label="Editor"
          about="How code looks and is formatted in the editor: theme, font, indentation, wrapping, formatters."
          state={editorSummary(editorSettings())}
        />
      </Group>

      <Group title="AI chats">
        <Item
          page="providers"
          label="Model providers"
          about="The models chats can use, with your API keys. The AI reviewer and the subagents pick theirs here too."
          state={ai(() => providersSummary(aiProviders()))}
          warning={aiSettingsLoaded() && aiProviders().length === 0}
        />
        <Item
          page="connectors"
          label="Connectors"
          about="Coding harnesses such as Claude Code and Codex, installed where a chat runs, used in place of a model: the chat hands them the task and shows their steps, tokens and cost."
          state={ai(() => connectorsSummary(aiProviders()))}
          warning={
            aiSettingsLoaded() &&
            splitSources(aiProviders()).connectors.some(
              (c) => !c.models.some((m) => m.enabled) || cantRun(connectorAuthState(c.id))
            )
          }
        />
        <Item
          page="mcp"
          label="MCP servers"
          about="Servers whose tools the agent can call, such as GitHub or a database: a command run on this computer, or a URL."
          state={ai(() => mcpServersSummary(aiMcpServers()))}
          warning={
            aiSettingsLoaded() &&
            aiMcpServers().some((s) => s.enabled && unsavedSecrets(s).length > 0)
          }
        />
        <Item
          page="permissions"
          label="Permissions & approval"
          about="What the agent may run and change in every chat, and what waits for your approval, or an AI reviewer’s, first."
          state={ai(() => permissionsSummary(aiPermissions()))}
        />
        <Item
          page="agent"
          label="Agent runs"
          about="As a chat starts, while the agent works and when it finishes: a branch for a new chat, pauses, its checklist, the checks on its changes, conflicts between chats, the files it edits."
          state={ai(() => agentRunsSummary(aiPermissions()))}
        />
        <Item
          page="subagents"
          label="Subagents"
          about="Helpers the agent can send to explore, review, triage a failure, write tests or carry out plan steps, each in a fresh context."
          state={ai(() => subagentSummary(aiSubagents()))}
        />
        <Item
          page="autopilot"
          label="Autopilot"
          about="Chats that carry out a whole task without asking: how thorough they are, when a task stops, the commands it never runs."
          state={ai(() => autopilotSummary(aiAutopilot()))}
        />
      </Group>

      <Show when={props.rootPath}>
        {(root) => (
          <Group title={`This project · ${folderName(root())}`}>
            <Item
              page="project"
              label="Project checks"
              about="The commands that check the agent’s changes in this project: type check, lint, tests, build."
              state={ai(() => projectChecksSummary(aiPermissions().verifyChanges, checksRunning()))}
              warning={
                aiSettingsLoaded() && (!aiPermissions().verifyChanges || checksRunning() === 0)
              }
            />
            <Item
              page="files"
              label="Restricted files"
              about="Files the agent may not read or change in this project: .env files, keys and other credentials, and any you add."
              state={restricted() ? restrictedSummary(restricted()!) : undefined}
            />
          </Group>
        )}
      </Show>

      <Group title="This computer">
        <Item
          page="workload"
          label="Heavy commands"
          about="How many builds, tests and type checks run at once, from all chats and the terminal, so the machine stays responsive."
          state={workload() ? workloadSummary(workload()!) : undefined}
        />
        {/* On Windows, WSL serves them. */}
        <Show when={!onWindows()}>
          <Item
            page="vfs"
            label="Virtual files"
            about="The filesystem driver that lets each chat work on its own copy of your changes, and the agent manage its own context."
            state={vfsLine()?.text}
            warning={vfsLine()?.warning}
          />
        </Show>
        <Show when={onWindows()}>
          <Item
            page="wsl"
            label="WSL"
            about="The WSL distribution the agent’s commands run in, sandboxed: Rovik’s Linux side on this computer."
            state={wslLine()?.text}
            warning={wslLine()?.warning}
          />
        </Show>
        <Item
          page="network"
          label="Network"
          about="How Rovik connects to model providers, the servers it downloads from and cloud hosts: how long a connection may take, and the proxy."
          state={network() ? networkSummary(network()!) : undefined}
        />
        <Item
          page="hosts"
          label="Cloud hosts"
          about="Servers your chats can move to and keep running on, over SSH, with what they may do there while you’re away."
          state={hosts() ? hostsSummary(hosts()!) : undefined}
        />
        <Item
          page="updates"
          label="Updates"
          about="New versions of Rovik: whether it looks for one every 30 minutes, and installing it here and on your cloud hosts."
          state={updatesSummary(updateInfo(), updateStatus(), checkingForUpdates())}
        />
      </Group>

      <p class="settings-menu-note">
        Chosen per chat, above its message box: the model, the mode (Agent, Plan or Autopilot), the
        reasoning effort and the context mode.
      </p>
    </div>
  );
}
