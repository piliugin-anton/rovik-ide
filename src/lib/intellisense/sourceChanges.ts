// sourceChanges.ts
//
// Changes to the project's sources on disk, applied together: once
// `quietMs` pass without another (or `maxWaitMs` after the first, while
// they keep coming), in one update. A burst of more than `burstLimit`
// files (a build, a branch switch) isn't read file by file: the sources are
// loaded again once `reloadQuietMs` pass without changes.
export interface SourceChangeSink {
  /** Applies these files: content if known, null when deleted, undefined to read. */
  apply(changes: ReadonlyMap<string, string | null | undefined>): Promise<void>;
  /** Loads every source again. */
  reload(): Promise<void>;
}

export interface SourceChangeTiming {
  quietMs: number;
  maxWaitMs: number;
  burstLimit: number;
  reloadQuietMs: number;
}

export const DEFAULT_TIMING: SourceChangeTiming = {
  quietMs: 500,
  maxWaitMs: 3_000,
  burstLimit: 20,
  reloadQuietMs: 5_000,
};

export class SourceChangeQueue {
  private readonly pending = new Map<string, string | null | undefined>();
  private firstQueuedAt: number | null = null;
  private flushTimer: ReturnType<typeof setTimeout> | undefined;
  /** A burst was skipped: changes wait for the reload. */
  private stale = false;
  private reloadTimer: ReturnType<typeof setTimeout> | undefined;

  constructor(
    private readonly sink: SourceChangeSink,
    private readonly timing: SourceChangeTiming = DEFAULT_TIMING
  ) {}

  /** A file changed: `content` if known, null when deleted, undefined to read it. */
  queue(path: string, content?: string | null): void {
    if (this.stale) {
      this.scheduleReload();
      return;
    }
    this.pending.set(path, content);
    this.firstQueuedAt ??= Date.now();
    clearTimeout(this.flushTimer);
    const { quietMs, maxWaitMs } = this.timing;
    const wait = Math.max(0, Math.min(quietMs, this.firstQueuedAt + maxWaitMs - Date.now()));
    this.flushTimer = setTimeout(() => void this.flush(), wait);
  }

  /**
   * Too much changed to say what (the watcher's "re-check everything"):
   * every source is loaded again once changes settle, as after a burst.
   */
  reloadAll(): void {
    clearTimeout(this.flushTimer);
    this.pending.clear();
    this.firstQueuedAt = null;
    this.stale = true;
    this.scheduleReload();
  }

  dispose(): void {
    clearTimeout(this.flushTimer);
    clearTimeout(this.reloadTimer);
    this.pending.clear();
  }

  private async flush(): Promise<void> {
    const changes = new Map(this.pending);
    this.pending.clear();
    this.firstQueuedAt = null;
    if (changes.size === 0) return;
    if (changes.size > this.timing.burstLimit) {
      this.stale = true;
      this.scheduleReload();
      return;
    }
    await this.sink.apply(changes);
  }

  private scheduleReload(): void {
    clearTimeout(this.reloadTimer);
    this.reloadTimer = setTimeout(() => {
      this.stale = false;
      void this.sink.reload();
    }, this.timing.reloadQuietMs);
  }
}
