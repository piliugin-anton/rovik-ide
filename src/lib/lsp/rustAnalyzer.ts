// rustAnalyzer.ts
//
// Rust support in the editor: finds rust-analyzer, offers to download it,
// runs it for the open project, and keeps the app's copy up to date.
//
//   - Started when a Rust file is first opened in a project.
//   - Which binary: a custom path (preferences), else the app's own copy or
//     one on PATH/rustup (src-tauri/src/lsp/rust_analyzer.rs).
//   - None: one notification offers the download (about 15 MB). It's
//     checked against GitHub's published SHA-256 before it's ever run.
//   - The app's copy is checked for updates once a day; a new version is
//     offered, and updating stops the running server, starts the new one
//     and opens the Rust files with it again.
//   - Without the standard library's sources (rust-src), hover and go to
//     definition into std don't work: offered once, installed with rustup.
import { Channel, invoke } from '@tauri-apps/api/core';
import { dismiss, notify } from '../notifications';
import { LspClient, type ServerStatus } from './client';
import { sharedMap } from '../sharedState';

interface RaBinary {
  path: string;
  version: string;
  source: 'managed' | 'system' | 'custom';
  tag: string | null;
}

interface RaRelease {
  tag: string;
  asset: string;
  url: string;
  sha256: string;
}

/** This machine's choices, not per project: shared by the windows
 *  (lib/sharedState.ts), so a window's daily update check doesn't undo a
 *  path or a choice another window saved. */
interface Prefs {
  /** Use this rust-analyzer binary. */
  customPath?: string;
  /** Prefer one on PATH/rustup over the app's copy. */
  preferSystem?: boolean;
  /** Check the app's copy for updates (default on). */
  checkForUpdates?: boolean;
  lastUpdateCheck?: number;
  /** "Skip this version". */
  skippedTag?: string;
}

const PREFS_KEY = 'rovik.rustAnalyzer';
const UPDATE_EVERY_MS = 24 * 60 * 60 * 1000;
/** How often to see whether a daily check is due. */
const UPDATE_POLL_MS = 60 * 60 * 1000;
/** Indexing shorter than this doesn't flash a notification. */
const STATUS_DELAY_MS = 1500;

const NOTE_INSTALL = 'ra-install';
const NOTE_STATUS = 'ra-status';
const NOTE_UPDATE = 'ra-update';

const prefs = sharedMap('rust-analyzer', { key: PREFS_KEY });

function loadPrefs(): Prefs {
  return { ...prefs.all() } as Prefs;
}

/** Only what changed (undefined removes it). */
function savePrefs(patch: Partial<Prefs>) {
  void prefs.set(patch);
}

/* ---------------------------------------------------------------------------
 * State
 * ------------------------------------------------------------------------- */

let current: { folders: readonly string[]; client: LspClient; binary: RaBinary } | null = null;
/**
 * The folders Rust support was started (or offered) for: a project's one, or
 * a workspace's several (C07). Replaced as a whole, and compared by key.
 */
let activeFolders: readonly string[] | null = null;
/** The folders as one key: the same folders in the same order are one workspace. */
const foldersKey = (folders: readonly string[]) => folders.join('\n');
const activeKey = () => (activeFolders === null ? null : foldersKey(activeFolders));
/** "Not now" on the download offer: don't ask again this session. */
let downloadDeclined = false;
/** Install or update in progress. */
let installing = false;
let updateTimer: number | undefined;
let statusTimer: number | undefined;
const rustSrcOffered = new Set<string>();
/** Health warnings already shown: each once per session, not on every change. */
const warningsShown = new Set<string>();

/**
 * A Rust file was opened in the window's folders: start rust-analyzer for
 * them if it isn't running for them already (a project's one folder, or a
 * workspace's several, which it reads as one workspace).
 */
export function ensureRustSupport(folders: readonly string[]) {
  if (folders.length === 0 || foldersKey(folders) === activeKey()) return;
  activeFolders = folders;
  void (async () => {
    await stopRustSupport({ keepRoot: true });
    const binary = await find();
    if (activeKey() !== foldersKey(folders)) return;
    if (!binary) {
      offerDownload(folders);
      return;
    }
    await launch(folders, binary);
  })().catch((err) => {
    notify({
      id: NOTE_INSTALL,
      kind: 'error',
      title: 'Rust support couldn’t start',
      message: String(err),
    });
  });
}

/**
 * The window's folders that are there changed (one came back, or left): a
 * server started for the old ones starts again for these (C07). With no
 * server started, there's nothing to change.
 */
export function workspaceFoldersChanged(folders: readonly string[]) {
  if (activeFolders === null) return;
  if (folders.length === 0) {
    void stopRustSupport();
    return;
  }
  ensureRustSupport(folders);
}

/** The window's folders closed or changed: stop the server. */
export async function stopRustSupport(opts: { keepRoot?: boolean } = {}) {
  if (!opts.keepRoot) activeFolders = null;
  window.clearInterval(updateTimer);
  window.clearTimeout(statusTimer);
  updateTimer = undefined;
  dismiss(NOTE_STATUS);
  const running = current;
  current = null;
  await running?.client.stop();
}

/** A file was saved: rust-analyzer checks the project (cargo check) on save. */
export function rustFileSaved(path: string) {
  if (path.endsWith('.rs')) current?.client.didSave(path);
}

/** Restarts rust-analyzer for the window's folders (finding the binary again). */
export function restartRustAnalyzer() {
  const folders = activeFolders;
  if (!folders) return;
  void (async () => {
    const binary = await find();
    if (!binary) {
      downloadDeclined = false;
      offerDownload(folders);
      return;
    }
    await restartWith(folders, binary);
  })();
}

/** Checks for a newer rust-analyzer now, and says so either way. */
export function checkForRustAnalyzerUpdate() {
  void checkForUpdate({ manual: true });
}

/* ---------------------------------------------------------------------------
 * Finding and starting
 * ------------------------------------------------------------------------- */

async function find(): Promise<RaBinary | null> {
  const prefs = loadPrefs();
  try {
    const binary = await invoke<RaBinary | null>('ra_find', {
      customPath: prefs.customPath ?? null,
      preferSystem: !!prefs.preferSystem,
    });
    return binary;
  } catch (err) {
    // The custom path doesn't work: say so instead of quietly using another.
    notify({
      id: NOTE_INSTALL,
      kind: 'error',
      title: 'The rust-analyzer set in preferences doesn’t run',
      message: String(err),
    });
    return null;
  }
}

async function launch(folders: readonly string[], binary: RaBinary) {
  const client = new LspClient({
    name: 'rust-analyzer',
    command: binary.path,
    folders,
    languageId: 'rust',
    extensions: ['.rs'],
    initializationOptions: {
      // It watches the files itself: edits by the agent, git and other
      // tools are picked up without the editor relaying them.
      files: { watcher: 'server' },
    },
    experimental: { serverStatusNotification: true },
    onStatus: showStatus,
    onShowMessage: (type, message) => {
      if (type !== 'info') notify({ kind: type, title: 'rust-analyzer', message, timeout: 15_000 });
    },
    onExit: (code, expected) => {
      if (expected || current?.client !== client) return;
      current = null;
      dismiss(NOTE_STATUS);
      notify({
        id: NOTE_INSTALL,
        kind: 'error',
        title: 'rust-analyzer stopped',
        message: code === null ? 'It ended unexpectedly.' : `It ended with exit code ${code}.`,
        actions: [
          { label: 'Restart', primary: true, run: () => void restartWith(folders, binary) },
        ],
      });
    },
  });
  current = { folders, client, binary };
  await client.start();
  if (binary.source === 'managed') scheduleUpdateChecks();
  void offerRustSrc(folders[0]);
}

/** Stops the running server and starts `binary`, which opens the Rust files again. */
async function restartWith(folders: readonly string[], binary: RaBinary) {
  const running = current;
  current = null;
  dismiss(NOTE_STATUS);
  await running?.client.stop();
  if (activeKey() === foldersKey(folders)) await launch(folders, binary);
}

function showStatus(status: ServerStatus) {
  window.clearTimeout(statusTimer);
  if (status.state === 'indexing' || status.state === 'starting') {
    // Short bursts (typing) don't need a notification; indexing a project does.
    statusTimer = window.setTimeout(() => {
      notify({
        id: NOTE_STATUS,
        title:
          status.state === 'starting' ? 'rust-analyzer is starting' : 'rust-analyzer is indexing',
        message: status.message,
        progress: status.progress ?? 'busy',
      });
    }, STATUS_DELAY_MS);
  } else if (status.state === 'error') {
    notify({
      id: NOTE_STATUS,
      kind: 'warning',
      title: 'rust-analyzer has a problem with this project',
      message: status.message,
    });
  } else if (status.state === 'warning' && status.message && !warningsShown.has(status.message)) {
    // e.g. "Workspace … is using an outdated toolchain version": it works,
    // with limits the user should know about.
    warningsShown.add(status.message);
    notify({
      id: NOTE_STATUS,
      kind: 'warning',
      title: 'rust-analyzer',
      message: status.message.split('\n').slice(0, 4).join('\n'),
      timeout: 20_000,
    });
  } else {
    dismiss(NOTE_STATUS);
  }
}

/* ---------------------------------------------------------------------------
 * Installing and updating
 * ------------------------------------------------------------------------- */

function offerDownload(folders: readonly string[]) {
  if (downloadDeclined || installing) return;
  notify({
    id: NOTE_INSTALL,
    title: 'Rust support needs rust-analyzer',
    message:
      'Go to definition, hover, completions and errors in Rust files come from rust-analyzer. ' +
      'Download it from its official GitHub release (about 15 MB)? It’s checked before it’s used.',
    actions: [
      { label: 'Not now', run: () => (downloadDeclined = true) },
      { label: 'Download', primary: true, run: () => void install(folders) },
    ],
  });
}

const mb = (bytes: number) => `${(bytes / 1_000_000).toFixed(1)} MB`;

/** Downloads the latest release and (re)starts rust-analyzer with it. */
async function install(folders: readonly string[], known?: RaRelease) {
  if (installing) return;
  installing = true;
  const updating = current !== null;
  const note = notify({
    id: NOTE_INSTALL,
    title: updating ? 'Updating rust-analyzer…' : 'Downloading rust-analyzer…',
    progress: 'busy',
  });
  try {
    const release = known ?? (await invoke<RaRelease>('ra_latest_release'));
    note.update({ title: `Downloading rust-analyzer ${release.tag}…`, progress: 0 });
    const progress = new Channel<{ downloaded: number; total: number | null }>();
    progress.onmessage = ({ downloaded, total }) =>
      note.update({
        progress: total ? downloaded / total : 'busy',
        message: total ? `${mb(downloaded)} of ${mb(total)}` : mb(downloaded),
      });
    const binary = await invoke<RaBinary>('ra_install', { release, onProgress: progress });

    note.update({ title: 'Starting rust-analyzer…', message: undefined, progress: 'busy' });
    savePrefs({ lastUpdateCheck: Date.now(), skippedTag: undefined });
    dismiss(NOTE_UPDATE);
    // An update while a custom or system copy is in use doesn't switch to it.
    const chosen = (await find()) ?? binary;
    if (activeKey() === foldersKey(folders)) await restartWith(folders, chosen);
    note.update({
      kind: 'success',
      title: updating
        ? `rust-analyzer updated to ${release.tag}`
        : `rust-analyzer ${release.tag} is ready`,
      message:
        chosen.source === 'managed'
          ? undefined
          : `Your ${chosen.source} rust-analyzer is still the one in use (see preferences).`,
      progress: undefined,
      timeout: 5000,
    });
  } catch (err) {
    note.update({
      kind: 'error',
      title: updating
        ? 'rust-analyzer couldn’t be updated'
        : 'rust-analyzer couldn’t be downloaded',
      message: `${friendly(err)}${updating ? '\nThe current version keeps running.' : ''}`,
      progress: undefined,
      actions: [{ label: 'Try again', primary: true, run: () => void install(folders, known) }],
    });
  } finally {
    installing = false;
  }
}

function friendly(err: unknown): string {
  const text = String(err);
  if (/dns|connect|timed out|network|offline/i.test(text)) {
    return 'GitHub couldn’t be reached. Check the network connection.';
  }
  return text;
}

function scheduleUpdateChecks() {
  if (updateTimer !== undefined) return;
  void checkForUpdate({ manual: false });
  updateTimer = window.setInterval(() => void checkForUpdate({ manual: false }), UPDATE_POLL_MS);
}

async function checkForUpdate({ manual }: { manual: boolean }) {
  const prefs = loadPrefs();
  const running = current;
  if (!running || running.binary.source !== 'managed' || installing) {
    if (manual) {
      notify({
        kind: 'info',
        title: 'Updates are for the app’s own rust-analyzer',
        message: running
          ? `This project uses your ${running.binary.source} rust-analyzer.`
          : 'rust-analyzer isn’t running.',
        timeout: 6000,
      });
    }
    return;
  }
  if (!manual) {
    if (prefs.checkForUpdates === false) return;
    if (prefs.lastUpdateCheck && Date.now() - prefs.lastUpdateCheck < UPDATE_EVERY_MS) return;
  }
  let release: RaRelease;
  try {
    release = await invoke<RaRelease>('ra_latest_release');
  } catch (err) {
    if (manual)
      notify({
        kind: 'error',
        title: 'Couldn’t check for updates',
        message: friendly(err),
        timeout: 8000,
      });
    return; // offline and the like: try again next time
  }
  savePrefs({ lastUpdateCheck: Date.now() });
  const installed = running.binary.tag ?? '';
  // Tags are dates (2026-09-28), so newer sorts later.
  const newer = release.tag > installed;
  if (!newer || (!manual && release.tag === prefs.skippedTag)) {
    if (manual)
      notify({
        kind: 'success',
        title: `rust-analyzer ${installed} is the latest version`,
        timeout: 5000,
      });
    return;
  }
  notify({
    id: NOTE_UPDATE,
    title: `rust-analyzer ${release.tag} is available`,
    message: `You have ${installed}. Updating restarts it; your open Rust files stay open.`,
    actions: [
      { label: 'Skip this version', run: () => savePrefs({ skippedTag: release.tag }) },
      { label: 'Later', run: () => {} },
      { label: 'Update', primary: true, run: () => void install(running.folders, release) },
    ],
  });
}

/* ---------------------------------------------------------------------------
 * The standard library's sources
 * ------------------------------------------------------------------------- */

async function offerRustSrc(root: string) {
  if (rustSrcOffered.has(root)) return;
  rustSrcOffered.add(root);
  let status: { installed: boolean; rustup: boolean };
  try {
    status = await invoke('ra_rust_src_status', { root });
  } catch {
    return;
  }
  if (status.installed) return;
  const why =
    'Hover and go to definition into the standard library (std, core) won’t work without them.';
  if (!status.rustup) {
    notify({
      kind: 'warning',
      title: 'The Rust standard library’s sources aren’t installed',
      message: `${why} Install the rust-src component for your toolchain.`,
    });
    return;
  }
  notify({
    id: 'ra-rust-src',
    kind: 'warning',
    title: 'The Rust standard library’s sources aren’t installed',
    message: `${why} rustup can add them for this project’s toolchain.`,
    actions: [
      { label: 'Not now', run: () => {} },
      {
        label: 'Install',
        primary: true,
        run: async () => {
          const note = notify({
            id: 'ra-rust-src',
            title: 'Installing rust-src…',
            progress: 'busy',
          });
          try {
            await invoke('ra_install_rust_src', { root });
            // rust-analyzer reads the sysroot when it loads the workspace.
            await current?.client.request('rust-analyzer/reloadWorkspace', null).catch(() => {});
            note.update({
              kind: 'success',
              title: 'rust-src installed',
              progress: undefined,
              timeout: 4000,
            });
          } catch (err) {
            note.update({
              kind: 'error',
              title: 'rust-src couldn’t be installed',
              message: String(err),
              progress: undefined,
            });
          }
        },
      },
    ],
  });
}
