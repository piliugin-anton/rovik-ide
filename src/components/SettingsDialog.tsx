// SettingsDialog.tsx
//
// The app-wide Settings dialog (lib/appSettings.ts opens it): a menu of
// pages, each about one thing (components/SettingsMenu.tsx), and the pages.
// Every setting applies as soon as it's changed: the editor's live
// (lib/editorSettings.ts); the AI settings (providers, permissions,
// subagents, Autopilot) through lib/aiSettingsStore.ts, which saves them
// at once and tells the chat panel; the rest (project checks, heavy
// commands, virtual files, cloud hosts) by the backend. There's nothing to
// save on close. Sub-pages keep a Back bar pinned under the header,
// outside the scrolling body.
import { createSignal, Match, onCleanup, onMount, Show, Switch } from 'solid-js';
import { PAGE_TITLE, type SettingsPage } from '../lib/appSettings';
import {
  aiAutopilot,
  aiChatModel,
  aiPermissions,
  aiProviders,
  aiSettingsLoadFailed,
  aiSettingsLoaded,
  aiSubagents,
  reloadAiSettings,
  updateAutopilot,
  updatePermissions,
  updateProviders,
  updateSubagents,
} from '../lib/aiSettingsStore';
import SettingsMenu from './SettingsMenu';
import EditorSettings from './EditorSettings';
import ModelProvidersSettings, { type ProvidersSettingsController } from './ModelProvidersSettings';
import PermissionsSettings from './PermissionsSettings';
import AgentRunsSettings from './AgentRunsSettings';
import SubagentSettings from './SubagentSettings';
import AutopilotSettings from './AutopilotSettings';
import ProjectChecksSettings from './ProjectChecksSettings';
import HeavyCommandsSettings from './HeavyCommandsSettings';
import VirtualFilesSettings from './VirtualFilesSettings';
import CloudHostsSettings, { type HostsSettingsController } from './CloudHostsSettings';

/** The pages that edit the AI settings: shown once those are read. */
const AI_PAGES: SettingsPage[] = [
  'providers',
  'permissions',
  'agent',
  'subagents',
  'autopilot',
  'project',
];

interface SettingsDialogProps {
  /** Opens straight on a page (e.g. "Manage providers…" in the model picker). */
  initialPage?: SettingsPage;
  /** The open project, for its checks page; none without a project. */
  rootPath?: string | null;
  onClose: () => void;
}

export default function SettingsDialog(props: SettingsDialogProps) {
  const [page, setPage] = createSignal<SettingsPage>(props.initialPage ?? 'menu');
  const [providersApi, setProvidersApi] = createSignal<ProvidersSettingsController | null>(null);
  const [hostsApi, setHostsApi] = createSignal<HostsSettingsController | null>(null);

  // The saved settings as they are now: the chat panel may have changed
  // them (the chat's model, what a model's provider accepts) since the
  // dialog was last open.
  onMount(() => void reloadAiSettings().catch((err) => console.error(err)));

  const title = () => {
    if (page() === 'providers') return providersApi()?.draftTitle() ?? PAGE_TITLE.providers;
    if (page() === 'hosts') return hostsApi()?.title() ?? PAGE_TITLE.hosts;
    return PAGE_TITLE[page()];
  };

  const backLabel = () => {
    if (page() === 'providers' && providersApi()?.draftTitle() != null) return 'Model providers';
    if (page() === 'hosts' && hostsApi()?.title() != null) return 'Cloud hosts';
    return 'Settings';
  };

  function goBack() {
    const api = providersApi();
    if (page() === 'providers' && api?.draftTitle() != null) {
      // Back from the provider form returns to the list (like Cancel).
      api.closeDraft();
      return;
    }
    const hosts = hostsApi();
    if (page() === 'hosts' && hosts?.title() != null) {
      hosts.back();
      return;
    }
    setPage('menu');
  }

  /** A page that needs the AI settings, before they're read. */
  const waitingForSettings = () => AI_PAGES.includes(page()) && !aiSettingsLoaded();

  function onKeyDown(e: KeyboardEvent) {
    // A confirm dialog on top handles (and stops) its own Escape.
    if (e.key !== 'Escape' || e.defaultPrevented) return;
    e.preventDefault();
    if (page() === 'menu') props.onClose();
    else goBack();
  }
  onMount(() => window.addEventListener('keydown', onKeyDown));
  onCleanup(() => window.removeEventListener('keydown', onKeyDown));

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
          <button class="icon-button" onClick={() => props.onClose()} aria-label="Close">
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
          <Show when={aiSettingsLoadFailed() && AI_PAGES.includes(page())}>
            <p class="settings-status warning settings-load-failed" role="alert">
              The saved AI settings couldn’t be read, so defaults are shown here and changes to them
              aren’t saved.{' '}
              <button
                class="link-button"
                onClick={() => void reloadAiSettings().catch((err) => console.error(err))}
              >
                Try again
              </button>
            </p>
          </Show>
          <Switch>
            <Match when={page() === 'menu'}>
              <SettingsMenu rootPath={props.rootPath ?? null} onOpen={setPage} />
            </Match>
            <Match when={page() === 'editor'}>
              <EditorSettings />
            </Match>
            <Match when={waitingForSettings()}>
              <p class="settings-hint">Reading the saved settings…</p>
            </Match>
            <Match when={page() === 'providers'}>
              <ModelProvidersSettings
                providers={aiProviders()}
                onChange={updateProviders}
                controller={setProvidersApi}
              />
            </Match>
            <Match when={page() === 'permissions'}>
              <PermissionsSettings
                permissions={aiPermissions()}
                providers={aiProviders()}
                onChange={updatePermissions}
                onOpenPage={setPage}
              />
            </Match>
            <Match when={page() === 'agent'}>
              <AgentRunsSettings
                permissions={aiPermissions()}
                onChange={updatePermissions}
                rootPath={props.rootPath ?? null}
                onOpenPage={setPage}
              />
            </Match>
            <Match when={page() === 'subagents'}>
              <SubagentSettings
                settings={aiSubagents()}
                providers={aiProviders()}
                chatModel={aiChatModel()}
                onChange={updateSubagents}
                onOpenPage={setPage}
              />
            </Match>
            <Match when={page() === 'autopilot'}>
              <AutopilotSettings
                settings={aiAutopilot()}
                onChange={updateAutopilot}
                onOpenPage={setPage}
              />
            </Match>
            <Match when={page() === 'project' && props.rootPath}>
              <ProjectChecksSettings
                root={props.rootPath!}
                permissions={aiPermissions()}
                onPermissionsChange={updatePermissions}
                onOpenPage={setPage}
              />
            </Match>
            <Match when={page() === 'project'}>
              <p class="settings-hint">
                No project is open. The checks are set per project: open a folder first.
              </p>
            </Match>
            <Match when={page() === 'workload'}>
              <HeavyCommandsSettings />
            </Match>
            <Match when={page() === 'vfs'}>
              <VirtualFilesSettings />
            </Match>
            <Match when={page() === 'hosts'}>
              <CloudHostsSettings controller={setHostsApi} />
            </Match>
          </Switch>
        </div>
      </div>
    </div>
  );
}
