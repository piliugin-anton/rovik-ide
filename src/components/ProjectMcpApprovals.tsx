// ProjectMcpApprovals.tsx
//
// Settings → MCP servers' list of the servers projects' own files set up
// that the user allowed in their chats (projectMcpServers.ts,
// mcp-plan-2.md Q12), each with its project and file, and Revoke: the
// project's chats ask again before they use it, and the values given for
// its variables are forgotten.
import { createResource, createSignal, For, Show } from 'solid-js';
import type { ProjectSummary } from '../lib/chatApi';
import {
  projectApprovals,
  revokeServer,
  type McpApproval,
  type ProjectApprovals as Listed,
} from '../lib/projectMcpServers';
import { confirmDialog } from './ConfirmDialog';
import { SettingStatus } from './SettingsControls';

export default function ProjectMcpApprovals() {
  const [list, { refetch }] = createResource(() =>
    projectApprovals().catch((e): Listed[] => {
      console.error("The projects' MCP servers weren't listed:", e);
      return [];
    })
  );
  const [error, setError] = createSignal<string | null>(null);

  async function revoke(project: ProjectSummary, approval: McpApproval) {
    const confirmed = await confirmDialog({
      title: 'Revoke this MCP server?',
      message: (
        <>
          <strong>{approval.name}</strong> won’t run in the chats of {project.name} until you allow
          it again
          {(approval.variables ?? []).length > 0
            ? ', and the values of its variables are forgotten, but for those another server of the project still takes'
            : ''}
          .
        </>
      ),
      confirmLabel: 'Revoke',
      danger: true,
    });
    if (!confirmed) return;
    setError(null);
    try {
      await revokeServer(project.path, project.id, approval.hash);
    } catch (e) {
      setError(String(e));
    }
    void refetch();
  }

  return (
    <Show when={(list() ?? []).length > 0}>
      <section class="settings-section">
        <div class="settings-section-title">Projects’ own servers</div>
        <p class="settings-hint">
          Servers a project’s .mcp.json, .cursor/mcp.json or .vscode/mcp.json sets up, which you
          allowed in its chats. One whose file changes asks again.
        </p>
        <For each={list()}>
          {(listed) => (
            <For each={listed.approvals}>
              {(approval) => (
                <div class="profile-row mcp-project-server-row">
                  <div class="profile-row-info">
                    <span class="profile-row-label">{approval.name}</span>
                    <span class="profile-row-sub">
                      {listed.project.name} · {approval.file}
                    </span>
                  </div>
                  <div class="profile-row-actions">
                    <button class="danger" onClick={() => void revoke(listed.project, approval)}>
                      Revoke
                    </button>
                  </div>
                </div>
              )}
            </For>
          )}
        </For>
        <Show when={error()}>{(e) => <SettingStatus warning>{e()}</SettingStatus>}</Show>
      </section>
    </Show>
  );
}
