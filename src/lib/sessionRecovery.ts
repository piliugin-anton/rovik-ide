/**
 * Coordinates explicit recovery. A replacement may supply its initial open and
 * context; only ordinary session setup is retried when the project changes.
 */
export function sessionRecovery<Context>(source: {
  context(): Context;
  open(context: Context): Promise<void>;
  lost(): void;
  restore(): Promise<void>;
  busy(value: boolean): void;
}) {
  let pending: Promise<void> | undefined;
  let activeOpen = source.open;
  function recover(firstOpen = source.open, firstContext = source.context): Promise<void> {
    if (pending) {
      if (firstOpen === source.open || firstOpen === activeOpen) return pending;
      // A requested replacement waits for any reconnect already in flight. Its
      // fresh attempt is still useful if the older connection could not recover.
      const next = () => recover(firstOpen, firstContext);
      return pending.then(next, next);
    }
    activeOpen = firstOpen;
    source.busy(true);
    pending = (async () => {
      let context = firstContext();
      source.lost();
      let open = firstOpen;
      for (;;) {
        await open(context);
        // Only session attachment is repeated when the project changes, never
        // the destructive replacement that the user requested once.
        open = source.open;
        if (JSON.stringify(context) !== JSON.stringify(source.context())) {
          context = source.context();
          source.lost();
          continue;
        }
        await source.restore();
        if (JSON.stringify(context) === JSON.stringify(source.context())) return;
        context = source.context();
        source.lost();
      }
    })().finally(() => {
      pending = undefined;
      source.busy(false);
    });
    return pending;
  }
  return recover;
}
