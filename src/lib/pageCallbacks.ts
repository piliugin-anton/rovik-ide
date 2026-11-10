import type { EventScope } from './eventReplay';
import type { ForwardedPort } from './cloud';

/** One-shot action, with the ports needed before constructing its tab. */
export interface PageRequest {
  dialogId: string;
  leadId: string;
  projectId: string | null;
  address: string;
  inTab: boolean;
  ports: ForwardedPort[];
}
interface Source {
  scope(): EventScope;
  subscribe(scope: EventScope, handler: (page: PageRequest) => void): Promise<string>;
  unsubscribe(id: string): Promise<unknown>;
  applyPorts(page: PageRequest): void;
  openTab(address: string): void;
  openExternal(address: string): Promise<unknown>;
  failed(error: unknown): void;
}
/** Scope changes invalidate queued actions immediately, including late subscription replies. */
export function pageCallbacks(source: Source): { refresh(): void; stop(): void } {
  let generation = 0;
  let stopped = false;
  let active: string | undefined;
  const release = (id: string) => void source.unsubscribe(id).catch(source.failed);
  const clear = () => {
    if (active !== undefined) release(active);
    active = undefined;
  };
  return {
    refresh() {
      if (stopped) return;
      const version = ++generation;
      const scope = source.scope();
      const key = JSON.stringify(scope);
      const current = () =>
        !stopped && generation === version && key === JSON.stringify(source.scope());
      clear();
      void source
        .subscribe(scope, (page) => {
          if (!current()) return;
          try {
            source.applyPorts(page);
            if (page.inTab) source.openTab(page.address);
            else void source.openExternal(page.address).catch(source.failed);
          } catch (error) {
            source.failed(error);
          }
        })
        .then((id) => {
          if (current()) active = id;
          else release(id);
        })
        .catch((error) => {
          if (current()) source.failed(error);
        });
    },
    stop() {
      stopped = true;
      generation++;
      clear();
    },
  };
}
