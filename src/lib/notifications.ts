// notifications.ts
//
// Small notifications in the corner of the window, for things the app
// wants the user to know or decide outside any dialog: "Rust support needs
// rust-analyzer. Download?", download progress, "a new version is
// available". Shown by <NotificationHost>.
//
//   const n = notify({ id: 'ra', title: 'Downloading…', progress: 0 });
//   n.update({ progress: 0.5 });
//   n.close();
//
// A notification with the same id replaces the previous one, so a flow can
// keep one notification up to date instead of stacking new ones.
import { createSignal } from 'solid-js';

export interface NotificationAction {
  label: string;
  /** The highlighted choice. */
  primary?: boolean;
  /** Runs the action; the notification closes unless `keepOpen`. */
  run: () => void;
  keepOpen?: boolean;
}

export interface NotificationOptions {
  /** Same id: replaces the existing notification. */
  id?: string;
  kind?: 'info' | 'success' | 'warning' | 'error';
  title: string;
  message?: string;
  actions?: NotificationAction[];
  /** 0..1, or 'busy' for an indeterminate bar. */
  progress?: number | 'busy';
  /** Closes by itself after this many ms. */
  timeout?: number;
}

export interface AppNotification extends NotificationOptions {
  id: string;
}

const [list, setList] = createSignal<AppNotification[]>([]);
const timers = new Map<string, number>();
let nextId = 1;

export const notifications = list;

export interface NotificationHandle {
  id: string;
  update(patch: Partial<NotificationOptions>): void;
  close(): void;
}

export function notify(options: NotificationOptions): NotificationHandle {
  const id = options.id ?? `n${nextId++}`;
  const entry: AppNotification = { kind: 'info', ...options, id };
  setList((all) =>
    all.some((n) => n.id === id) ? all.map((n) => (n.id === id ? entry : n)) : [...all, entry]
  );
  schedule(entry);
  return {
    id,
    update: (patch) => {
      let updated: AppNotification | undefined;
      setList((all) => all.map((n) => (n.id === id ? (updated = { ...n, ...patch, id }) : n)));
      if (updated && patch.timeout !== undefined) schedule(updated);
    },
    close: () => dismiss(id),
  };
}

export function dismiss(id: string) {
  window.clearTimeout(timers.get(id));
  timers.delete(id);
  setList((all) => all.filter((n) => n.id !== id));
}

function schedule(n: AppNotification) {
  window.clearTimeout(timers.get(n.id));
  timers.delete(n.id);
  if (n.timeout)
    timers.set(
      n.id,
      window.setTimeout(() => dismiss(n.id), n.timeout)
    );
}
