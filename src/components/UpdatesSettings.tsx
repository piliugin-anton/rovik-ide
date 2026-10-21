// UpdatesSettings.tsx
//
// Settings → Updates (lib/updates.ts): the version this is, where its
// update stands, "Check now", and the switch for the looks every 30
// minutes, which every window follows (on unless turned off). A new
// version's Update is here as in its notification. The cloud hosts run
// their own Rovik: once this app runs a new version it offers them that
// version (lib/hostUpdates.ts).
import { createMemo, createSignal, onCleanup, Show } from 'solid-js';
import type { SettingsPage } from '../lib/appSettings';
import {
  checkForUpdates,
  checkingForUpdates,
  installUpdate,
  openReleasePage,
  restartForUpdate,
  setCheckForUpdates,
  statusText,
  updateInfo,
  updateStatus,
  type Release,
} from '../lib/updates';
import { SettingStatus } from './SettingsControls';

interface UpdatesSettingsProps {
  onOpenPage: (page: SettingsPage) => void;
}

export default function UpdatesSettings(props: UpdatesSettingsProps) {
  // "Looked 5 min ago" stays true while the page is open.
  const [now, setNow] = createSignal(Date.now());
  const clock = window.setInterval(() => setNow(Date.now()), 30_000);
  onCleanup(() => window.clearInterval(clock));

  const line = createMemo(() => statusText(updateStatus(), now()));
  const installable = () => updateInfo()?.installable !== false;
  /** The release a button acts on, while there's one. */
  const release = (): Release | null => {
    const s = updateStatus();
    if (s.kind === 'available' || s.kind === 'ready') return s.release;
    if (s.kind === 'failed') return s.release ?? null;
    return null;
  };
  const working = () => ['checking', 'downloading', 'installing'].includes(updateStatus().kind);

  return (
    <div class="permissions-settings updates-settings">
      <p class="settings-hint">
        New versions of Rovik come from its releases on GitHub, signed: Rovik installs only what its
        release key signed. Looking for one, and downloading it, go the way{' '}
        <button class="link-button" onClick={() => props.onOpenPage('network')}>
          Network
        </button>{' '}
        sends downloads.
      </p>

      <section class="settings-section">
        <div class="settings-section-title">This version</div>
        <p class="settings-hint updates-version">Rovik {updateInfo()?.version ?? '…'}</p>
        <Show when={line()}>
          {(l) => <SettingStatus warning={l().warning}>{l().text}</SettingStatus>}
        </Show>
        <div class="host-actions">
          <button disabled={working()} onClick={() => void checkForUpdates()}>
            {updateStatus().kind === 'checking' ? 'Checking…' : 'Check now'}
          </button>
          <Show when={release()}>
            {(r) => (
              <>
                <Show
                  when={installable()}
                  fallback={
                    <button class="chat-action-button primary" onClick={() => openReleasePage(r())}>
                      Download {r().version}
                    </button>
                  }
                >
                  <button
                    class="chat-action-button primary"
                    disabled={working()}
                    onClick={() => void installUpdate(r())}
                  >
                    {updateStatus().kind === 'ready'
                      ? 'Restart and update'
                      : `Update to ${r().version}`}
                  </button>
                </Show>
                <button class="link-button" onClick={() => openReleasePage(r())}>
                  What’s new
                </button>
              </>
            )}
          </Show>
          <Show when={updateStatus().kind === 'installed'}>
            <button class="chat-action-button primary" onClick={() => void restartForUpdate()}>
              Restart
            </button>
          </Show>
        </div>
        <Show when={!installable()}>
          <p class="settings-hint setting-explain">
            This copy of Rovik wasn’t installed from a release (a build of your own, say), so it
            doesn’t update itself: a new version is offered as a download.
            {updateInfo()?.periodic === false
              ? ' Being a development build, it looks only when you click Check now.'
              : ''}
          </p>
        </Show>
        <p class="settings-hint setting-explain">
          Updating restarts Rovik. Chats at work stop, once you say so, and you can continue them
          when it’s back; tabs with unsaved text are saved or left as you say. On Windows the
          installer closes every Rovik window, and starts Rovik again once it’s done.
        </p>
      </section>

      <section class="settings-section">
        <div class="settings-section-title">Looking for updates</div>
        <label class="setting-checkbox">
          <input
            type="checkbox"
            checked={checkingForUpdates()}
            onChange={(e) => setCheckForUpdates(e.currentTarget.checked)}
          />
          <span>
            Look for a new version every 30 minutes
            <span class="setting-checkbox-sub">
              A new version is offered in a notification, and nothing is downloaded until you choose
              Update. Off, Rovik looks only when you click Check now. Every Rovik window on this
              computer follows this.
            </span>
          </span>
        </label>
      </section>

      <section class="settings-section">
        <div class="settings-section-title">Cloud hosts</div>
        <p class="settings-hint">
          Each cloud host runs a Rovik of its own. Once this app runs a new version, it offers to
          bring your hosts to it: those you administer it updates for everyone there, whose runs
          pause while Rovik restarts and then go on; for the others it says whom to ask. A host’s
          page in{' '}
          <button class="link-button" onClick={() => props.onOpenPage('hosts')}>
            Cloud hosts
          </button>{' '}
          has “Upgrade for everyone” too.
        </p>
      </section>
    </div>
  );
}
