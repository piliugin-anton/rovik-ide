/** Coordinates explicit recovery. Only session setup is retried when its context changes. */
export function sessionRecovery<Context>(source: {
  context(): Context;
  open(context: Context): Promise<void>;
  lost(): void;
  restore(): Promise<void>;
  busy(value: boolean): void;
}) {
  let pending: Promise<void> | undefined;
  return () => {
    if (pending) return pending;
    source.busy(true);
    pending = (async () => {
      let context = source.context();
      source.lost();
      for (;;) {
        await source.open(context);
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
  };
}
