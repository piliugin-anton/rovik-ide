// AiSettingsModal.tsx
//
// The app-wide Settings dialog: a menu of pages (editor, model providers,
// permissions & approval, …). Sub-pages keep a Back bar pinned under the
// header, outside the scrolling body. AI changes are kept here and handed
// to the chat on close; editor changes apply live (lib/editorSettings.ts).
import { createSignal, Match, onCleanup, onMount, Show, Switch, type JSX } from 'solid-js';
import type { ModelSelection, Provider } from '../types';
import type { AgentPermissions, ApprovalRule } from '../lib/permissions';
import ModelProvidersSettings, { type ProvidersSettingsController } from './ModelProvidersSettings';
import PermissionsSettings from './PermissionsSettings';
import ProjectChecksSettings from './ProjectChecksSettings';
import VirtualFilesSettings from './VirtualFilesSettings';
import BuildsSettings from './BuildsSettings';
import SubagentSettings from './SubagentSettings';
import { subagentSummary, type SubagentSettings as SubagentChoices } from '../lib/subagentSettings';
import EditorSettings from './EditorSettings';
import { editorSettings, EDITOR_THEMES } from '../lib/editorSettings';
import { plural } from '../lib/textUtils';

export type AiSettingsPage =
  'menu' | 'editor' | 'providers' | 'permissions' | 'subagents' | 'project' | 'builds' | 'vfs';

interface AiSettingsModalProps {
  providers: Provider[];
  permissions: AgentPermissions;
  subagents: SubagentChoices;
  /** The chat's model, for the Subagents page's provider warning. */
  chatModel?: ModelSelection | null;
  /** Opens straight on a page (e.g. "Manage providers…" in the model picker). */
  initialPage?: AiSettingsPage;
  /** The open project, for its checks page; none without a project. */
  rootPath?: string | null;
  onSave: (changes: {
    providers: Provider[];
    permissions: AgentPermissions;
    subagents: SubagentChoices;
  }) => void;
  onClose: () => void;
}

const PAGE_TITLE: Record<AiSettingsPage, string> = {
  menu: 'Settings',
  editor: 'Editor',
  providers: 'Model providers',
  permissions: 'Permissions & Approval',
  subagents: 'Subagents',
  project: 'Project checks',
  builds: 'Builds & checks',
  vfs: 'Virtual files',
};

export default function AiSettingsModal(props: AiSettingsModalProps) {
  const [page, setPage] = createSignal<AiSettingsPage>(props.initialPage ?? 'menu');
  /** One line of the menu: opens its page. */
  const MenuItem = (item: { page: AiSettingsPage; label: string; sub: JSX.Element }) => (
    <button class="settings-menu-item" onClick={() => setPage(item.page)}>
      <span class="settings-menu-text">
        <span class="settings-menu-label">{item.label}</span>
        <span class="settings-menu-sub">{item.sub}</span>
      </span>
      <span class="settings-menu-chevron" aria-hidden="true">
        ›
      </span>
    </button>
  );
  const [providers, setProviders] = createSignal<Provider[]>(props.providers);
  const [permissions, setPermissions] = createSignal<AgentPermissions>(props.permissions);
  const [subagents, setSubagents] = createSignal<SubagentChoices>(props.subagents);
  const [providersApi, setProvidersApi] = createSignal<ProvidersSettingsController | null>(null);

  const title = () => {
    if (page() === 'providers') return providersApi()?.draftTitle() ?? PAGE_TITLE.providers;
    return PAGE_TITLE[page()];
  };

  const backLabel = () =>
    page() === 'providers' && providersApi()?.draftTitle() != null ? 'Model providers' : 'Settings';

  function goBack() {
    const api = providersApi();
    if (page() === 'providers' && api?.draftTitle() != null) {
      // Back from the provider form returns to the list (like Cancel).
      api.closeDraft();
      return;
    }
    setPage('menu');
  }

  function commitAndClose() {
    props.onSave({ providers: providers(), permissions: permissions(), subagents: subagents() });
    props.onClose();
  }

  function onKeyDown(e: KeyboardEvent) {
    // A confirm dialog on top handles (and stops) its own Escape.
    if (e.key !== 'Escape' || e.defaultPrevented) return;
    e.preventDefault();
    if (page() === 'menu') commitAndClose();
    else goBack();
  }
  onMount(() => window.addEventListener('keydown', onKeyDown));
  onCleanup(() => window.removeEventListener('keydown', onKeyDown));

  const providersSummary = () => {
    const list = providers();
    if (list.length === 0) return 'No providers yet';
    const models = list.reduce((n, p) => n + p.models.filter((m) => m.enabled).length, 0);
    return `${plural(list.length, 'provider')}, ${plural(models, 'model')} enabled`;
  };

  const permissionsSummary = () => {
    const p = permissions();
    const rule = (r: ApprovalRule) =>
      !r.required ? 'no approval' : r.by === 'ai' ? 'AI approval' : 'your approval';
    const commands = p.allowAnyCommand ? 'any' : `${p.allowedCommands.length} listed`;
    return `Commands: ${commands}, ${rule(p.commandApproval)} · Edits: ${rule(p.editApproval)} · Other tools: ${rule(p.toolApproval)}`;
  };

  return (
    <div class="modal-overlay">
      <div
        class="modal model-settings ai-settings"
        // The providers page holds the models table: full names need room.
        classList={{ wide: page() === 'providers' }}
        role="dialog"
        aria-modal="true"
        aria-label={title()}
      >
        <div class="modal-header">
          <span class="modal-title" title={title()}>
            {title()}
          </span>
          <button class="icon-button" onClick={commitAndClose} aria-label="Close">
            ×
          </button>
        </div>

        <Show when={page() !== 'menu'}>
          <div class="modal-back-bar">
            <button class="link-button modal-back-button" onClick={goBack}>
              <span aria-hidden="true">‹</span> {backLabel()}
            </button>
          </div>
        </Show>

        <div class="modal-body">
          <Switch>
            <Match when={page() === 'menu'}>
              <div class="settings-menu">
                <MenuItem
                  page="editor"
                  label="Editor"
                  sub={
                    <>
                      Theme:{' '}
                      {EDITOR_THEMES.find((t) => t.id === editorSettings().theme)?.label ??
                        editorSettings().theme}{' '}
                      · Font {editorSettings().fontSize}px ·{' '}
                      {editorSettings().insertSpaces
                        ? `Spaces (${editorSettings().tabSize})`
                        : 'Tabs'}
                    </>
                  }
                />
                <MenuItem page="providers" label="Model providers" sub={providersSummary()} />
                <Show when={props.rootPath}>
                  <MenuItem
                    page="project"
                    label="Project checks"
                    sub={
                      permissions().verifyChanges
                        ? 'Changes are checked when the agent finishes'
                        : 'Checking changes is off'
                    }
                  />
                </Show>
                <MenuItem
                  page="permissions"
                  label="Permissions & Approval"
                  sub={permissionsSummary()}
                />
                <MenuItem page="subagents" label="Subagents" sub={subagentSummary(subagents())} />
                <MenuItem
                  page="builds"
                  label="Builds & checks"
                  sub="How many builds and tests run at once, across chats"
                />
                <MenuItem
                  page="vfs"
                  label="Virtual files"
                  sub="In-memory files commands can use, via FUSE / macFUSE / WinFsp"
                />
              </div>
            </Match>
            <Match when={page() === 'editor'}>
              <EditorSettings />
            </Match>
            <Match when={page() === 'providers'}>
              <ModelProvidersSettings
                providers={providers()}
                onChange={setProviders}
                controller={setProvidersApi}
              />
            </Match>
            <Match when={page() === 'project' && props.rootPath}>
              <ProjectChecksSettings
                root={props.rootPath!}
                permissions={permissions()}
                onPermissionsChange={setPermissions}
              />
            </Match>
            <Match when={page() === 'permissions'}>
              <PermissionsSettings
                permissions={permissions()}
                providers={providers()}
                onChange={setPermissions}
                onOpenProviders={() => setPage('providers')}
              />
            </Match>
            <Match when={page() === 'subagents'}>
              <SubagentSettings
                settings={subagents()}
                providers={providers()}
                chatModel={props.chatModel ?? null}
                onChange={setSubagents}
              />
            </Match>
            <Match when={page() === 'builds'}>
              <BuildsSettings />
            </Match>
            <Match when={page() === 'vfs'}>
              <VirtualFilesSettings />
            </Match>
          </Switch>
        </div>
      </div>
    </div>
  );
}
