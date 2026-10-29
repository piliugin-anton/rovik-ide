// ProjectMcpApprovals.tsx
//
// Settings → MCP servers' list of the servers projects' own files set up
// that the user allowed in their chats (projectMcpServers.ts,
// mcp-plan-2.md Q12), each with its project and file, and Revoke: the
// project's chats ask again before they use it, and the values given for
// its variables are forgotten. One that may ask for a sign-in says where
// it's signed in, with Sign in and Sign out (Q14), as the user's own do.
import { createResource, createSignal, For, Show } from 'solid-js';
import type { ProjectSummary } from '../lib/chatApi';
import type { HostRow } from '../lib/hosts';
import { createSignIns, projectServerId, signInProjectServer } from '../lib/mcpSignIn';
import {
  projectApprovals,
  revokeServer,
  type McpApproval,
  type ProjectApprovals as Listed,
} from '../lib/projectMcpServers';
import { confirmDialog } from './ConfirmDialog';
import { SettingStatus } from './SettingsControls';

export interface ProjectMcpApprovalsProps {
  /**
   * The user's hosts, as the page has them (undefined while it reads
   * them): those that keep sign-ins get one too.
   */
  hosts: HostRow[] | undefined;
}

export default function ProjectMcpApprovals(props: ProjectMcpApprovalsProps) {
  const [list, { refetch }] = createResource(() =>
    projectApprovals().catch((e): Listed[] => {
      console.error("The projects' MCP servers weren't listed:", e);
      return [];
    })
  );
  const [error, setError] = createSignal<string | null>(null);
  const idOf = (project: ProjectSummary, approval: McpApproval) =>
    projectServerId(project.id, approval.hash);
  const signIns = createSignIns(
    () =>
      (list() ?? []).flatMap((l) =>
        l.approvals.filter((a) => a.signsIn).map((a) => idOf(l.project, a))
      ),
    () => props.hosts
  );

  async function signIn(project: ProjectSummary, approval: McpApproval) {
    setError(
      await signIns.signIn(idOf(project, approval), approval.name, (place) =>
        signInProjectServer(project.path, project.id, approval.hash, place)
      )
    );
  }

  async function signOut(project: ProjectSummary, approval: McpApproval) {
    const id = idOf(project, approval);
    const confirmed = await confirmDialog({
      title: 'Sign out of this MCP server?',
      message: (
        <>
          <strong>{approval.name}</strong> is signed out everywhere it’s signed in:{' '}
          {signIns.said(signIns.at(id))}. The chats of {project.name} can’t use it until you sign in
          again.
        </>
      ),
      confirmLabel: 'Sign out',
    });
    if (!confirmed) return;
    setError(await signIns.signOut(id));
  }

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
          {signIns.at(idOf(project, approval)).length > 0 ? ', and it’s signed out' : ''}.
        </>
      ),
      confirmLabel: 'Revoke',
      danger: true,
    });
    if (!confirmed) return;
    setError(null);
    // Its sign-ins go with it (Q14), first: one a host still keeps can be
    // retried while it's listed.
    if (signIns.at(idOf(project, approval)).length > 0) {
      const failed = await signIns.signOut(idOf(project, approval));
      if (failed) {
        setError(failed);
        return;
      }
    }
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
                    <Show when={signIns.at(idOf(listed.project, approval)).length > 0}>
                      <span class="profile-row-sub">
                        Signed in: {signIns.said(signIns.at(idOf(listed.project, approval)))}
                      </span>
                    </Show>
                  </div>
                  <div class="profile-row-actions">
                    <Show
                      when={
                        approval.signsIn &&
                        signIns.missing(idOf(listed.project, approval)).length > 0
                      }
                    >
                      <button
                        title={`Sign in for ${signIns.said(signIns.missing(idOf(listed.project, approval)))}, if it asks you to`}
                        disabled={!!signIns.progress()}
                        onClick={() => void signIn(listed.project, approval)}
                      >
                        Sign in
                      </button>
                    </Show>
                    <Show when={signIns.at(idOf(listed.project, approval)).length > 0}>
                      <button
                        disabled={!!signIns.progress()}
                        onClick={() => void signOut(listed.project, approval)}
                      >
                        Sign out
                      </button>
                    </Show>
                    <button class="danger" onClick={() => void revoke(listed.project, approval)}>
                      Revoke
                    </button>
                  </div>
                </div>
              )}
            </For>
          )}
        </For>
        <Show when={signIns.progress()}>{(text) => <SettingStatus>{text()}</SettingStatus>}</Show>
        <Show when={error()}>{(e) => <SettingStatus warning>{e()}</SettingStatus>}</Show>
      </section>
    </Show>
  );
}
