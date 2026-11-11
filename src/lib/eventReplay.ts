/** A journal reader retries reads only. A checkpoint is committed after application. */
export interface EventScope {
  projectId: string | null;
  chats: string[];
}
export interface JournalEvent {
  channel: 'agent' | 'cloud';
  payload: unknown;
}
export interface EventBatch {
  cursor: unknown;
  reset: boolean;
  events: JournalEvent[];
}
export interface ReplaySource {
  scope(): EventScope;
  /** A new command session invalidates in-flight reads even if the project is unchanged. */
  version?(): unknown;
  poll(scope: EventScope, cursor: unknown): Promise<EventBatch>;
  refresh(scope: EventScope, current: () => boolean): Promise<void>;
  apply(event: JournalEvent): void;
  failed(error: unknown): void;
  recovered(): void;
  pause(): Promise<void>;
}
/** Stop prevents late replies from changing stores after the window is disposed. */
export function followEvents(source: ReplaySource): { stop(): void; done: Promise<void> } {
  let stopped = false;
  const done = (async () => {
    let cursor: unknown = null;
    while (!stopped) {
      const scope = source.scope();
      const version = source.version?.();
      const current = () =>
        !stopped &&
        version === source.version?.() &&
        JSON.stringify(scope) === JSON.stringify(source.scope());
      try {
        const batch = await source.poll(scope, cursor);
        if (!current()) {
          cursor = null;
          continue;
        }
        if (batch.reset) await source.refresh(scope, current);
        if (!current()) {
          cursor = null;
          continue;
        }
        try {
          for (const event of batch.events) source.apply(event);
        } catch (error) {
          cursor = null;
          throw error;
        }
        cursor = batch.cursor;
        source.recovered();
      } catch (error) {
        if (stopped) break;
        source.failed(error);
        await source.pause();
      }
    }
  })();
  return {
    stop: () => {
      stopped = true;
    },
    done,
  };
}
