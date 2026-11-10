/** Serializes this window's priority reports, coalescing changes while a call is pending. */
export function workloadFocus(source: {
  chat(): string | null;
  send(dialogId: string | null): Promise<unknown>;
  failed(error: unknown): void;
}): { refresh(): void; stop(): void } {
  let stopped = false;
  let running = false;
  let pending = false;
  let next: string | null = null;
  const pump = async () => {
    if (running) return;
    running = true;
    try {
      while (pending) {
        const dialog = next;
        pending = false;
        try {
          await source.send(dialog);
        } catch (error) {
          source.failed(error);
        }
      }
    } finally {
      running = false;
    }
  };
  const enqueue = (dialog: string | null) => {
    next = dialog;
    pending = true;
    void pump();
  };
  return {
    refresh() {
      if (!stopped) enqueue(source.chat());
    },
    stop() {
      if (stopped) return;
      stopped = true;
      enqueue(null);
    },
  };
}
