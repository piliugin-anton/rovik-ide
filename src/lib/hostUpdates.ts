// hostUpdates.ts
//
// The cloud hosts, once Rovik runs a new version (lib/updates.ts updated it,
// or it was installed by hand). Each deployed host whose Rovik is older is
// looked at once it has connected: its daemon says its version, and whether
// you're one of its administrators. Then one notification says which hosts
// can be brought to this app's version. "Update hosts" upgrades the ones
// you administer, one at a time, as Settings → Cloud hosts' "Upgrade for
// everyone" does: Rovik there is replaced for all its users, whose daemons
// pause their runs and continue them on the new build. For the others it
// says whom to ask. Once per version of the app, whichever window looks
// first (lib/sharedState.ts). WSL isn't a cloud host: it's brought to the
// app's version at the start, without asking (wslStartup.ts).

import { invoke } from '@tauri-apps/api/core';
import { openAppSettings } from './appSettings';
import {
  asDeployFailure,
  hostDetails,
  listHosts,
  upgradeHost,
  type HostDetails,
  type HostRow,
  type HostsList,
} from './hosts';
import { notify, type NotificationAction } from './notifications';
import { sharedMap, type SharedMap } from './sharedState';
import { isNewer, type UpdateInfo } from './updates';

/** How long the hosts get to connect after the start before they're looked at. */
const CONNECT_WAIT_MS = 90_000;
/** How often their connections are looked at meanwhile. */
const CONNECT_POLL_MS = 5_000;
export const HOSTS_NOTE = 'host-updates';

/** Shared by the windows. */
interface Prefs {
  /** The app version the hosts were last looked at for. */
  lookedFor?: string;
}

/** A host with an older Rovik than this app. */
export interface HostBehind {
  id: string;
  name: string;
  /** Rovik's version there. */
  version: string;
  /**
   * Connected: true when you're one of its administrators, who can update
   * it; false with whom to ask. Unset: not connected, so not known.
   */
  admin?: boolean;
  admins?: string[];
}

/** The deployed hosts whose Rovik, as last known, is older than `appVersion`. */
export function hostsBehindIn(list: HostsList, appVersion: string): HostRow[] {
  return list.hosts.filter(
    (h) => h.deploy.state === 'deployed' && isNewer(appVersion, h.deploy.version)
  );
}

/** Still connecting: not to be looked at yet. */
const connecting = (row: HostRow) => row.state === undefined || row.state === 'connecting';

/** A host as its daemon says it now; null once it's as new as the app (someone upgraded it). */
export function behindAs(row: HostRow, details: HostDetails | null): HostBehind | null {
  const kept = row.deploy.state === 'deployed' ? row.deploy.version : '';
  if (!details) return { id: row.id, name: row.name, version: kept };
  if (!details.upgrade) return null;
  return {
    id: row.id,
    name: row.name,
    version: details.info.version,
    admin: details.admin,
    admins: details.info.install?.admins ?? [],
  };
}

/** "A", "A and B", "A, B and C". */
export function namesOf(hosts: { name: string }[]): string {
  const names = hosts.map((h) => h.name);
  if (names.length < 2) return names[0] ?? '';
  return `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`;
}

/** The notification's words for `behind`; null when there's nothing to say. */
export function hostsNotice(
  behind: HostBehind[],
  appVersion: string
): { title: string; message: string; canUpdate: HostBehind[] } | null {
  if (behind.length === 0) return null;
  const canUpdate = behind.filter((h) => h.admin === true);
  const ask = behind.filter((h) => h.admin === false);
  const unknown = behind.filter((h) => h.admin === undefined);
  const lines: string[] = [];
  if (canUpdate.length > 0) {
    lines.push(
      `${namesOf(canUpdate)} can be updated to ${appVersion}, for everyone there: runs pause while Rovik restarts, then go on.`
    );
  }
  for (const host of ask) {
    const who = host.admins?.length ? host.admins.join(' or ') : 'its administrator';
    lines.push(`${host.name} runs ${host.version}: ask ${who} to update it.`);
  }
  if (unknown.length > 0) {
    const one = unknown.length === 1;
    lines.push(
      `${namesOf(unknown)} ${one ? 'isn’t' : 'aren’t'} connected: update ${one ? 'it' : 'them'} in Settings → Cloud hosts once ${one ? 'it’s' : 'they’re'} back.`
    );
  }
  const title =
    behind.length === 1
      ? `${behind[0].name} runs an older Rovik (${behind[0].version})`
      : `${behind.length} cloud hosts run an older Rovik`;
  return { title, message: lines.join('\n'), canUpdate };
}

export interface HostUpdaterDeps {
  appInfo: () => Promise<UpdateInfo>;
  listHosts: () => Promise<HostsList>;
  hostDetails: (id: string) => Promise<HostDetails>;
  upgradeHost: (id: string) => Promise<void>;
  prefs: SharedMap;
  now: () => number;
  sleep: (ms: number) => Promise<void>;
}

export function createHostUpdater(deps: HostUpdaterDeps) {
  let started = false;
  let upgrading = false;

  /** Once per window start: the hosts looked at for this app's version, if no window has yet. */
  async function start() {
    if (started) return;
    started = true;
    let app: UpdateInfo;
    try {
      app = await deps.appInfo();
    } catch {
      return; // not the app
    }
    const lookedFor = async () => {
      await deps.prefs.refresh();
      return (deps.prefs.all() as Prefs).lookedFor;
    };
    if ((await lookedFor()) === app.version) return;
    const behind = await look(app.version);
    if (behind === null) return;
    // Looked at for this version: not again, here or in another window,
    // unless another window got there first meanwhile.
    if ((await lookedFor()) === app.version) return;
    await deps.prefs.set({ lookedFor: app.version });
    tell(behind, app.version);
  }

  /** The hosts behind `appVersion`, once they've had their time to connect; null if they can't be listed. */
  async function look(appVersion: string): Promise<HostBehind[] | null> {
    const deadline = deps.now() + CONNECT_WAIT_MS;
    let rows: HostRow[];
    for (;;) {
      let list: HostsList;
      try {
        list = await deps.listHosts();
      } catch {
        return null;
      }
      rows = hostsBehindIn(list, appVersion);
      if (!rows.some(connecting) || deps.now() >= deadline) break;
      await deps.sleep(CONNECT_POLL_MS);
    }
    const behind: HostBehind[] = [];
    for (const row of rows) {
      let details: HostDetails | null = null;
      if (row.state === 'ready') {
        // Its daemon says what it runs now: someone may have upgraded it.
        details = await deps.hostDetails(row.id).catch(() => null);
      }
      const host = behindAs(row, details);
      if (host) behind.push(host);
    }
    return behind;
  }

  function tell(behind: HostBehind[], appVersion: string) {
    const notice = hostsNotice(behind, appVersion);
    if (!notice) return;
    const actions: NotificationAction[] = [
      { label: 'Show hosts', run: () => openAppSettings('hosts') },
      { label: 'Later', run: () => {} },
    ];
    if (notice.canUpdate.length > 0) {
      actions.push({
        label: notice.canUpdate.length === 1 ? 'Update host' : 'Update hosts',
        primary: true,
        run: () => void update(notice.canUpdate, appVersion),
      });
    }
    notify({ id: HOSTS_NOTE, kind: 'info', title: notice.title, message: notice.message, actions });
  }

  /** Upgrades `hosts`, one at a time, then says how it went. */
  async function update(hosts: HostBehind[], appVersion: string) {
    if (upgrading) return;
    upgrading = true;
    const failed: { host: HostBehind; message: string }[] = [];
    try {
      for (const [i, host] of hosts.entries()) {
        notify({
          id: HOSTS_NOTE,
          title: `Updating ${host.name}…`,
          message:
            hosts.length > 1
              ? `${i + 1} of ${hosts.length}: Rovik ${appVersion} there, for everyone.`
              : `Rovik ${appVersion} there, for everyone.`,
          progress: 'busy',
        });
        try {
          await deps.upgradeHost(host.id);
        } catch (err) {
          failed.push({ host, message: asDeployFailure(err).message });
        }
      }
    } finally {
      upgrading = false;
    }
    const updated = hosts.filter((h) => !failed.some((f) => f.host.id === h.id));
    if (failed.length === 0) {
      notify({
        id: HOSTS_NOTE,
        kind: 'success',
        title: `${namesOf(updated)} ${updated.length === 1 ? 'runs' : 'run'} Rovik ${appVersion}`,
        timeout: 8000,
      });
      return;
    }
    const lines = failed.map((f) => `${f.host.name}: ${f.message}`);
    if (updated.length > 0) lines.unshift(`${namesOf(updated)}: updated.`);
    notify({
      id: HOSTS_NOTE,
      kind: 'warning',
      title:
        failed.length === 1
          ? `${failed[0].host.name} couldn’t be updated`
          : `${failed.length} hosts couldn’t be updated`,
      message: lines.join('\n'),
      actions: [
        { label: 'Show hosts', primary: true, run: () => openAppSettings('hosts') },
        { label: 'Close', run: () => {} },
      ],
    });
  }

  return { start, look, update };
}

const hostUpdater = createHostUpdater({
  appInfo: () => invoke<UpdateInfo>('update_info'),
  listHosts,
  hostDetails,
  upgradeHost,
  prefs: sharedMap('host-updates', { key: 'rovik.hostUpdates' }),
  now: () => Date.now(),
  sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
});

/** Once, from App.tsx, in the background. */
export const startHostUpdates = () => hostUpdater.start();
