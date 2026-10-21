// updates.ts
//
// Rovik's own updates (src-tauri/src/updates.rs, over tauri-plugin-updater).
// While Settings → Updates allows it (on unless turned off), the latest
// release is looked at every 30 minutes, the first time a moment after the
// start, and a newer one is offered in a notification. Its Update
// downloads the release, checks it against the release key and installs
// it, then Rovik starts again on it, this window on its project. What
// starting again would lose is asked about first: chats at work, which
// stop, and tabs with unsaved text (the app's own save dialog).
//
// The windows share the checks (lib/sharedState.ts): one window's check is
// every window's, so Rovik looks once every 30 minutes however many windows
// are open, and a version one window installed is offered to the others as
// a restart, not a second download. A copy of Rovik not installed from a
// release (a build of one's own) is told of a new release, to download.
//
// Once Rovik runs a newer version, the cloud hosts are offered it too
// (lib/hostUpdates.ts).

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { createSignal } from 'solid-js';
import { listHosts } from './hosts';
import { notify, type NotificationAction } from './notifications';
import { sharedMap, type SharedMap } from './sharedState';
import { formatBytes, plural } from './textUtils';

/** This app's version, and whether it updates itself (updates.rs `Info`). */
export interface UpdateInfo {
  version: string;
  installable: boolean;
  /** It looks every 30 minutes: not a development build, which looks when asked. */
  periodic: boolean;
}

/** A release newer than this app, as a check found it (updates.rs `Available`). */
export interface Available {
  version: string;
  current: string;
  /** When it was published, in ms. */
  published?: number;
  page: string;
  installable: boolean;
}

/** A release, as the windows share it. */
export interface Release {
  version: string;
  /** Its page: what's new, and its downloads. */
  page: string;
  published?: number;
}

/** What starting again would stop (updates.rs `AtWork`). */
export interface AtWork {
  /** Chats this window is at work on. */
  here: number;
  /** Where installing closes every Rovik window (Windows): the other windows' chats at work, */
  elsewhere: number;
  /** and whether any other window is open. */
  otherWindows: boolean;
  closesAll: boolean;
}

/** How a download goes (`app-update-progress`). */
export interface Progress {
  version: string;
  downloaded: number;
  total: number | null;
}

/** Where this window stands, for Settings → Updates. */
export type UpdateStatus =
  | { kind: 'idle' }
  | { kind: 'checking' }
  | { kind: 'upToDate'; at: number }
  | { kind: 'available'; release: Release }
  | { kind: 'downloading'; release: Release; done: number; total: number | null }
  /** Downloaded, waiting for the go to start again. */
  | { kind: 'ready'; release: Release }
  | { kind: 'installing'; release: Release }
  /** Installed, by this window or another: this one runs the old version until it starts again. */
  | { kind: 'installed'; version: string }
  | { kind: 'failed'; message: string; release?: Release };

/** What starting again needs of the window (App.tsx gives it). */
export interface UpdateHooks {
  /** The project open in the window, to open again; null without one. */
  folder: () => string | null;
  /** Tabs with unsaved text saved or let go, as the user says, and the tabs kept: false if they cancelled. */
  settle: () => Promise<boolean>;
  /** Asks the user: true for yes. */
  confirm: (options: {
    title: string;
    message: string;
    confirmLabel: string;
    cancelLabel?: string;
  }) => Promise<boolean>;
}

/** Shared by the windows. */
export interface UpdatePrefs {
  /** Look for updates every 30 minutes: on unless turned off. */
  checkForUpdates?: boolean;
  /** When a window last looked, in ms. */
  lastCheck?: number;
  /** The newest release a window's check found newer than its app. */
  latest?: Release;
  /** The version a window installed: the others start again on it. */
  installed?: string;
}

/** How often the latest release is looked at. */
export const CHECK_EVERY_MS = 30 * 60 * 1000;
/** How often a window sees whether a look is due: another window's may have been. */
const POLL_MS = 5 * 60 * 1000;
/** The first look, a moment after the start: the window comes first. */
const FIRST_LOOK_MS = 20_000;
/** The notification, kept up to date through each step. */
export const UPDATE_NOTE = 'app-update';

/**
 * Whether version `a` comes after `b`, number by number: 1.2.10 after
 * 1.2.9, and 1.2.3 after its pre-releases (1.2.3-4).
 */
export function isNewer(a: string, b: string): boolean {
  const parse = (v: string) => {
    const [main, pre] = v.trim().replace(/^v/, '').split('-', 2);
    return {
      numbers: main.split('.').map((n) => Number.parseInt(n, 10) || 0),
      pre: pre === undefined ? null : pre,
    };
  };
  const x = parse(a);
  const y = parse(b);
  for (let i = 0; i < Math.max(x.numbers.length, y.numbers.length); i++) {
    const d = (x.numbers[i] ?? 0) - (y.numbers[i] ?? 0);
    if (d !== 0) return d > 0;
  }
  if (x.pre === y.pre) return false;
  if (x.pre === null) return true;
  if (y.pre === null) return false;
  const [p, q] = [Number(x.pre), Number(y.pre)];
  return Number.isNaN(p) || Number.isNaN(q) ? x.pre > y.pre : p > q;
}

/** What a window's poll does: look at the latest release, or go by what another window's look found. */
export type PollStep = 'off' | 'check' | 'restart' | 'offer' | 'none';

export function pollStep(prefs: UpdatePrefs, version: string, now: number): PollStep {
  // Installed by another window, the user's own doing: not a look's.
  if (prefs.installed && isNewer(prefs.installed, version)) return 'restart';
  if (prefs.checkForUpdates === false) return 'off';
  const last = prefs.lastCheck;
  // A clock set back makes a look from "the future" due too.
  if (!last || now - last >= CHECK_EVERY_MS || now < last) return 'check';
  if (prefs.latest && isNewer(prefs.latest.version, version)) return 'offer';
  return 'none';
}

/** What to tell the user before starting again stops something; null when nothing would stop. */
export function restartWarning(atWork: AtWork): string | null {
  const parts: string[] = [];
  if (atWork.here > 0) {
    parts.push(
      `${plural(atWork.here, 'chat')} in this window ${atWork.here === 1 ? 'is' : 'are'} at work: ${atWork.here === 1 ? 'it stops' : 'they stop'}, and you can continue ${atWork.here === 1 ? 'it' : 'them'} once Rovik is back.`
    );
  }
  if (atWork.closesAll && (atWork.otherWindows || atWork.elsewhere > 0)) {
    const chats =
      atWork.elsewhere > 0
        ? `, and the ${plural(atWork.elsewhere, 'chat')} at work there ${atWork.elsewhere === 1 ? 'stops' : 'stop'}`
        : '';
    parts.push(
      `The installer also closes Rovik’s other windows${chats}: save what’s open in them first.`
    );
  }
  return parts.length > 0 ? parts.join(' ') : null;
}

const releaseOf = (found: Available): Release => ({
  version: found.version,
  page: found.page,
  ...(found.published !== undefined ? { published: found.published } : {}),
});

/** What the updater needs, given (tests give their own). */
export interface UpdaterDeps {
  invoke: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
  /** Calls `fn` with each step of a download until the function it resolves with is called. */
  onProgress: (fn: (progress: Progress) => void) => Promise<() => void>;
  prefs: SharedMap;
  now: () => number;
  /** Opens a release's page in the browser. */
  openPage: (url: string) => void;
  /** The cloud hosts deployed with a Rovik older than `version`, by name. */
  hostsBehind: (version: string) => Promise<string[]>;
}

/** What an update offer says of the cloud hosts it leaves behind (lib/hostUpdates.ts offers them it after). */
export function hostsLine(names: string[]): string {
  if (names.length === 0) return '';
  const them = names.length === 1 ? names[0] : `your ${plural(names.length, 'cloud host')}`;
  return `, then offers ${them} the new version too`;
}

export function createUpdater(deps: UpdaterDeps) {
  const [info, setInfo] = createSignal<UpdateInfo | null>(null);
  const [status, setStatus] = createSignal<UpdateStatus>({ kind: 'idle' });
  const prefsNow = () => deps.prefs.all() as UpdatePrefs;
  const [enabled, setEnabled] = createSignal(prefsNow().checkForUpdates !== false);
  let hooks: UpdateHooks | null = null;
  /** Releases this window offered: each once, until it starts again. */
  const offered = new Set<string>();
  /** A download, an install or a restart is under way. */
  let busy = false;
  let started = false;

  // Another window's look, setting or install, as this one comes to the front.
  deps.prefs.subscribe((all) => {
    setEnabled((all as UpdatePrefs).checkForUpdates !== false);
    void poll();
  });

  function savePrefs(patch: Partial<UpdatePrefs>) {
    void deps.prefs.set(patch);
  }

  /** Once, as the window starts: what this app is, then a look now and then. */
  async function start(given: UpdateHooks, timers = true) {
    if (started) return;
    started = true;
    hooks = given;
    let app: UpdateInfo;
    try {
      app = await deps.invoke<UpdateInfo>('update_info');
    } catch {
      return; // not the app (a browser)
    }
    setInfo(app);
    if (!timers || !app.periodic) return;
    window.setTimeout(() => void poll(), FIRST_LOOK_MS);
    window.setInterval(() => void poll(), POLL_MS);
  }

  /** A look if one is due; else what another window's last look found. */
  async function poll() {
    const app = info();
    if (!app || !app.periodic || busy) return;
    const prefs = prefsNow();
    switch (pollStep(prefs, app.version, deps.now())) {
      case 'check':
        return check({ manual: false });
      case 'restart':
        return offerRestart(prefs.installed!);
      case 'offer':
        setStatus({ kind: 'available', release: prefs.latest! });
        return offer(prefs.latest!, false);
    }
  }

  /** Looks at the latest release now; `manual` says what it found, even when it's nothing. */
  async function check({ manual }: { manual: boolean }) {
    const app = info();
    if (!app || busy) return;
    setStatus({ kind: 'checking' });
    let found: Available | null;
    try {
      found = await deps.invoke<Available | null>('update_check');
    } catch (err) {
      // Offline, say: the next look tries again.
      savePrefs({ lastCheck: deps.now() });
      setStatus({ kind: 'failed', message: String(err) });
      if (manual) {
        notify({
          id: UPDATE_NOTE,
          kind: 'error',
          title: 'Couldn’t check for updates',
          message: String(err),
          timeout: 10_000,
        });
      }
      return;
    }
    const at = deps.now();
    if (!found) {
      savePrefs({ lastCheck: at });
      const installed = prefsNow().installed;
      if (installed && isNewer(installed, app.version)) return offerRestart(installed);
      setStatus({ kind: 'upToDate', at });
      if (manual) {
        notify({
          id: UPDATE_NOTE,
          kind: 'success',
          title: `Rovik ${app.version} is the latest version`,
          timeout: 5000,
        });
      }
      return;
    }
    const release = releaseOf(found);
    savePrefs({ lastCheck: at, latest: release });
    const installed = prefsNow().installed;
    if (installed && !isNewer(release.version, installed) && isNewer(installed, app.version)) {
      return offerRestart(installed);
    }
    setStatus({ kind: 'available', release });
    await offer(release, manual);
  }

  /** The notification for `release`: once per window, unless asked for. */
  async function offer(release: Release, manual: boolean) {
    const app = info();
    if (!app || (!manual && offered.has(release.version))) return;
    offered.add(release.version);
    const whatsNew: NotificationAction = {
      label: 'What’s new',
      keepOpen: true,
      run: () => deps.openPage(release.page),
    };
    const later: NotificationAction = { label: 'Later', run: () => {} };
    if (!app.installable) {
      notify({
        id: UPDATE_NOTE,
        title: `Rovik ${release.version} is available`,
        message: `You have ${app.version}. This copy of Rovik wasn’t installed from a release, so it doesn’t update itself: download the new version from its page.`,
        actions: [
          later,
          { label: 'Download', primary: true, run: () => deps.openPage(release.page) },
        ],
      });
      return;
    }
    const hosts = await deps.hostsBehind(release.version).catch(() => []);
    notify({
      id: UPDATE_NOTE,
      title: `Rovik ${release.version} is available`,
      message: `You have ${app.version}. Updating restarts Rovik${hostsLine(hosts)}.`,
      actions: [
        whatsNew,
        later,
        { label: 'Update', primary: true, run: () => void install(release) },
      ],
    });
  }

  /** Another window (or this one, earlier) installed `version`: this one starts again on it. */
  function offerRestart(version: string) {
    const app = info();
    if (!app) return;
    setStatus({ kind: 'installed', version });
    if (offered.has(`installed ${version}`)) return;
    offered.add(`installed ${version}`);
    notify({
      id: UPDATE_NOTE,
      kind: 'success',
      title: `Rovik ${version} is installed`,
      message: `This window still runs ${app.version}: it uses the new version once it starts again.`,
      actions: [
        { label: 'Later', run: () => {} },
        { label: 'Restart', primary: true, run: () => void restart() },
      ],
    });
  }

  /**
   * Whether starting again may go ahead: the user said yes to what it would
   * stop (chats at work), and the tabs are kept, their unsaved text saved
   * or let go as they say.
   */
  async function readyToRestart(): Promise<boolean> {
    if (!hooks) return false;
    let atWork: AtWork | null = null;
    try {
      atWork = await deps.invoke<AtWork>('update_at_work');
    } catch {
      // Not known: the save dialog still comes.
    }
    const warning = atWork && restartWarning(atWork);
    if (warning) {
      const go = await hooks.confirm({
        title: 'Restart Rovik now?',
        message: warning,
        confirmLabel: 'Restart now',
        cancelLabel: 'Not now',
      });
      if (!go) return false;
    }
    return hooks.settle();
  }

  /** Downloads `release`, then, once the user is ready, installs it and starts Rovik again on it. */
  async function install(release: Release) {
    const app = info();
    if (!app || !hooks || busy) return;
    busy = true;
    try {
      setStatus({ kind: 'downloading', release, done: 0, total: null });
      const note = notify({
        id: UPDATE_NOTE,
        title: `Downloading Rovik ${release.version}…`,
        progress: 'busy',
      });
      const stop = await deps.onProgress((p) => {
        if (p.version !== release.version) return;
        setStatus({ kind: 'downloading', release, done: p.downloaded, total: p.total });
        note.update({
          progress: p.total ? p.downloaded / p.total : 'busy',
          message: p.total
            ? `${formatBytes(p.downloaded)} of ${formatBytes(p.total)}`
            : formatBytes(p.downloaded),
        });
      });
      try {
        await deps.invoke('update_download', { version: release.version });
      } finally {
        stop();
      }
      setStatus({ kind: 'ready', release });
      if (!(await readyToRestart())) {
        notify({
          id: UPDATE_NOTE,
          title: `Rovik ${release.version} is downloaded`,
          message: 'It’s installed when Rovik restarts for it.',
          actions: [
            { label: 'Later', run: () => {} },
            { label: 'Restart and update', primary: true, run: () => void install(release) },
          ],
        });
        return;
      }
      setStatus({ kind: 'installing', release });
      notify({
        id: UPDATE_NOTE,
        title: `Installing Rovik ${release.version}…`,
        message: 'Rovik starts again once it’s installed.',
        progress: 'busy',
      });
      // On Windows the installer takes over here: the app ends.
      await deps.invoke('update_install', { version: release.version });
      savePrefs({ installed: release.version });
      setStatus({ kind: 'installed', version: release.version });
      // A package's password prompt may have taken a while, and a tab
      // changed meanwhile: settled again (nothing to ask if none did).
      if (!(await hooks.settle())) {
        offerRestart(release.version);
        return;
      }
      await deps.invoke('update_restart', { folder: hooks.folder() });
    } catch (err) {
      setStatus({ kind: 'failed', message: String(err), release });
      notify({
        id: UPDATE_NOTE,
        kind: 'error',
        title: `Rovik ${release.version} couldn’t be installed`,
        message: String(err),
        actions: [
          { label: 'Download it', run: () => deps.openPage(release.page) },
          { label: 'Try again', primary: true, run: () => void install(release) },
        ],
      });
    } finally {
      busy = false;
    }
  }

  /** Starts this window again on the version installed, once the user is ready. */
  async function restart() {
    if (!hooks || busy) return;
    busy = true;
    try {
      if (await readyToRestart()) {
        await deps.invoke('update_restart', { folder: hooks.folder() });
      }
    } catch (err) {
      notify({
        id: UPDATE_NOTE,
        kind: 'error',
        title: 'Rovik couldn’t start again',
        message: String(err),
      });
    } finally {
      busy = false;
    }
  }

  /** Settings → Updates' switch: on looks soon, off stops the looks (in every window). */
  function setCheckForUpdates(on: boolean) {
    setEnabled(on);
    savePrefs({ checkForUpdates: on ? undefined : false });
    if (on) void poll();
  }

  return {
    info,
    status,
    enabled,
    start,
    poll,
    check,
    install,
    restart,
    setCheckForUpdates,
    openPage: (release: Release) => deps.openPage(release.page),
  };
}

const updater = createUpdater({
  invoke: (cmd, args) => invoke(cmd, args),
  onProgress: (fn) => listen<Progress>('app-update-progress', (e) => fn(e.payload)),
  prefs: sharedMap('app-updates', { key: 'rovik.appUpdates' }),
  now: () => Date.now(),
  openPage: (url) => {
    invoke('open_url', { url }).catch((err) => console.error('Open link:', err));
  },
  hostsBehind: async (version) => {
    const { hosts } = await listHosts();
    return hosts
      .filter((h) => h.deploy.state === 'deployed' && isNewer(version, h.deploy.version))
      .map((h) => h.name);
  },
});

/** This app's version and whether it updates itself; null until known. */
export const updateInfo = updater.info;
/** Where this window's update stands. */
export const updateStatus = updater.status;
/** Settings → Updates' switch: the 30-minute looks are on. */
export const checkingForUpdates = updater.enabled;
export const setCheckForUpdates = updater.setCheckForUpdates;
/** Once, from App.tsx: the looks start (`timers` off: none but those asked for, for tests). */
export const startUpdates = (hooks: UpdateHooks, timers = true) => updater.start(hooks, timers);
/** "Check now": what it finds is said, nothing included. */
export const checkForUpdates = () => updater.check({ manual: true });
export const installUpdate = (release: Release) => updater.install(release);
export const restartForUpdate = () => updater.restart();
/** A release's page in the browser: what's new, and its downloads. */
export const openReleasePage = (release: Release) => updater.openPage(release);

/** How long ago `at` was, roughly: "just now", "5 min ago", "2 h ago". */
export function agoText(at: number, now: number): string {
  const minutes = Math.floor(Math.max(0, now - at) / 60_000);
  if (minutes < 1) return 'just now';
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  return hours < 24 ? `${hours} h ago` : plural(Math.floor(hours / 24), 'day') + ' ago';
}

/** What Settings → Updates says of where this window stands; null before anything's known. */
export function statusText(
  status: UpdateStatus,
  now: number
): { text: string; warning?: boolean } | null {
  switch (status.kind) {
    case 'idle':
      return null;
    case 'checking':
      return { text: 'Looking for a new version…' };
    case 'upToDate':
      return { text: `This is the latest version (looked ${agoText(status.at, now)}).` };
    case 'available':
      return { text: `Rovik ${status.release.version} is available.` };
    case 'downloading': {
      const { done, total } = status;
      const amount = total ? `${formatBytes(done)} of ${formatBytes(total)}` : formatBytes(done);
      return { text: `Downloading Rovik ${status.release.version}: ${amount}.` };
    }
    case 'ready':
      return {
        text: `Rovik ${status.release.version} is downloaded: it’s installed when Rovik restarts for it.`,
      };
    case 'installing':
      return { text: `Installing Rovik ${status.release.version}…` };
    case 'installed':
      return {
        text: `Rovik ${status.version} is installed: this window uses it once it starts again.`,
      };
    case 'failed':
      return { text: status.message, warning: true };
  }
}

/** One line for the Settings menu: the version, and where its update stands. */
export function updatesSummary(info: UpdateInfo | null, status: UpdateStatus, on: boolean): string {
  const looks = on && info?.periodic !== false;
  const now = (): string => {
    switch (status.kind) {
      case 'available':
      case 'ready':
        return `${status.release.version} is available`;
      case 'downloading':
      case 'installing':
        return `updating to ${status.release.version}`;
      case 'installed':
        return `${status.version} installed: restart to use it`;
    }
    return looks ? 'looks for updates every 30 minutes' : 'doesn’t look for updates';
  };
  const line = [info ? `Rovik ${info.version}` : null, now()].filter(Boolean).join(' · ');
  return line.charAt(0).toUpperCase() + line.slice(1);
}
