// AiSettingsModal.tsx
//
// The chat's settings: a menu of pages (model providers, permissions &
// approval). Sub-pages keep a Back bar pinned under the header, outside the
// scrolling body. Changes are kept here and handed to the chat on close.
import { createSignal, Match, onCleanup, onMount, Show, Switch } from 'solid-js';
import type { Provider } from '../types';
import type { AgentPermissions, ApprovalRule } from '../lib/permissions';
import ModelProvidersSettings, { type ProvidersSettingsController } from './ModelProvidersSettings';
import PermissionsSettings from './PermissionsSettings';

export type AiSettingsPage = 'menu' | 'providers' | 'permissions';

interface AiSettingsModalProps {
  providers: Provider[];
  permissions: AgentPermissions;
  /** Opens straight on a page (e.g. "Manage providers…" in the model picker). */
  initialPage?: AiSettingsPage;
  onSave: (changes: { providers: Provider[]; permissions: AgentPermissions }) => void;
  onClose: () => void;
}

const PAGE_TITLE: Record<AiSettingsPage, string> = {
  menu: 'AI Chat settings',
  providers: 'Model providers',
  permissions: 'Permissions & Approval',
};

export default function AiSettingsModal(props: AiSettingsModalProps) {
  const [page, setPage] = createSignal<AiSettingsPage>(props.initialPage ?? 'menu');
  const [providers, setProviders] = createSignal<Provider[]>(props.providers);
  const [permissions, setPermissions] = createSignal<AgentPermissions>(props.permissions);
  const [providersApi, setProvidersApi] = createSignal<ProvidersSettingsController | null>(null);

  const title = () => {
    if (page() === 'providers') return providersApi()?.draftTitle() ?? PAGE_TITLE.providers;
    return PAGE_TITLE[page()];
  };

  const backLabel = () =>
    page() === 'providers' && providersApi()?.draftTitle() != null
      ? 'Model providers'
      : 'AI Chat settings';

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
    props.onSave({ providers: providers(), permissions: permissions() });
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
    return `${list.length} provider${list.length === 1 ? '' : 's'}, ${models} model${models === 1 ? '' : 's'} enabled`;
  };

  const permissionsSummary = () => {
    const p = permissions();
    const rule = (r: ApprovalRule) =>
      !r.required ? 'no approval' : r.by === 'ai' ? 'AI approval' : 'your approval';
    const commands = p.allowAnyCommand ? 'any' : `${p.allowedCommands.length} listed`;
    return `Commands: ${commands}, ${rule(p.commandApproval)} · Edits: ${rule(p.editApproval)} · Other tools: ${rule(p.toolApproval)}`;
  };

  return (
    <div
      class="modal-overlay"
      onMouseDown={(e) => {
        // Only a press that starts on the backdrop closes, so a drag out of
        // the modal (or a click on a confirm dialog above it) doesn't.
        if (e.target === e.currentTarget) commitAndClose();
      }}
    >
      <div
        class="modal model-settings ai-settings"
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
                <button class="settings-menu-item" onClick={() => setPage('providers')}>
                  <span class="settings-menu-text">
                    <span class="settings-menu-label">Model providers</span>
                    <span class="settings-menu-sub">{providersSummary()}</span>
                  </span>
                  <span class="settings-menu-chevron" aria-hidden="true">
                    ›
                  </span>
                </button>
                <button class="settings-menu-item" onClick={() => setPage('permissions')}>
                  <span class="settings-menu-text">
                    <span class="settings-menu-label">Permissions &amp; Approval</span>
                    <span class="settings-menu-sub">{permissionsSummary()}</span>
                  </span>
                  <span class="settings-menu-chevron" aria-hidden="true">
                    ›
                  </span>
                </button>
              </div>
            </Match>
            <Match when={page() === 'providers'}>
              <ModelProvidersSettings
                providers={providers()}
                onChange={setProviders}
                controller={setProvidersApi}
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
          </Switch>
        </div>
      </div>
    </div>
  );
}
