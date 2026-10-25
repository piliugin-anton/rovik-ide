// connectorUpdates.ts
//
// A connector's program, once a newer version of it is published. The
// check as a window starts (lib/connectors.ts) finds the newest version of
// each connector's program; one notification then offers to update those
// behind, once per version, whichever window finds it first
// (lib/sharedState.ts). "Update" runs each program's own updater on this
// computer (`claude update`: the step "Update to …" runs in Settings →
// Connectors, lib/connectorSetup.ts), checks it again, and says how it went.

import type { Provider } from '../types';
import { openAppSettings } from './appSettings';
import { checkConnector, errorText, type CheckedState } from './connectors';
import { runSetupStep, setupCall } from './connectorSetup';
import { notify } from './notifications';
import { sharedMap, type Entries, type SharedMap } from './sharedState';

export const CONNECTOR_UPDATES_NOTE = 'connector-updates';

/** A program a newer version of is published, and the connector to update it with. */
export interface Outdated {
  /** The first connector that runs it. */
  connector: Provider;
  /** Where it's installed: one program, however many connectors run it. */
  program: string;
  /** As the program says it: `2.1.310 (Claude Code)`. */
  version: string;
  /** The newest published: `2.2.0`. */
  update: string;
}

/**
 * The programs the check found a newer version of, each with the first
 * connector that runs it; not those whose newest version was offered
 * already (`offered`: the version last offered, by program).
 */
export function outdated(
  providers: Provider[],
  found: Record<string, CheckedState>,
  offered: Entries
): Outdated[] {
  const seen = new Set<string>();
  const list: Outdated[] = [];
  for (const connector of providers) {
    const probe = found[connector.id]?.probe;
    if (!probe?.update || seen.has(probe.program)) continue;
    seen.add(probe.program);
    if (offered[probe.program] === probe.update) continue;
    list.push({ connector, program: probe.program, version: probe.version, update: probe.update });
  }
  return list;
}

/** Why the check after an update says it didn't work; null when it did. */
function stillBehind(after: CheckedState): string | null {
  if (!after.probe) return after.state === 'unknown' ? after.message : 'It didn’t answer.';
  const { version, update } = after.probe;
  return update ? `Still ${version}: it didn’t update to ${update}.` : null;
}

export interface ConnectorUpdaterDeps {
  /** Shared by the windows: the version last offered, by program. */
  prefs: SharedMap;
  /** Runs the connector's update step on this computer to its end; throws what failed. */
  runUpdate: (connector: Provider) => Promise<unknown>;
  /** Checks the connector again on this computer, and keeps what it found. */
  check: (connector: Provider) => Promise<CheckedState>;
  openConnectors: () => void;
}

export function createConnectorUpdater(deps: ConnectorUpdaterDeps) {
  let updating = false;

  /** After the start's check: offers the newer versions it found, each once. */
  async function offer(providers: Provider[], found: Record<string, CheckedState>) {
    await deps.prefs.refresh();
    const list = outdated(providers, found, deps.prefs.all());
    if (list.length === 0) return;
    // Offered: not again, here or in another window.
    await deps.prefs.set(Object.fromEntries(list.map((o) => [o.program, o.update])));
    const one = list.length === 1 ? list[0] : null;
    notify({
      id: CONNECTOR_UPDATES_NOTE,
      kind: 'info',
      title: one
        ? `${one.connector.name} ${one.update} is available`
        : `Newer versions of ${list.length} connectors are available`,
      message: one
        ? `You have ${one.version}.`
        : list.map((o) => `${o.connector.name}: ${o.version} → ${o.update}`).join('\n'),
      actions: [
        { label: 'Later', run: () => {} },
        { label: 'Update', primary: true, run: () => void update(list) },
      ],
    });
  }

  /** Updates each program, one at a time, then says how it went. */
  async function update(list: Outdated[]) {
    if (updating) return;
    updating = true;
    const updated: { o: Outdated; version: string }[] = [];
    const failed: { o: Outdated; message: string }[] = [];
    try {
      for (const [i, o] of list.entries()) {
        notify({
          id: CONNECTOR_UPDATES_NOTE,
          title: `Updating ${o.connector.name}…`,
          message:
            list.length > 1 ? `${i + 1} of ${list.length}: to ${o.update}.` : `To ${o.update}.`,
          progress: 'busy',
        });
        try {
          await deps.runUpdate(o.connector);
          const after = await deps.check(o.connector);
          const behind = stillBehind(after);
          if (behind === null) updated.push({ o, version: after.probe?.version ?? o.update });
          else failed.push({ o, message: behind });
        } catch (err) {
          failed.push({ o, message: errorText(err) });
        }
      }
    } finally {
      updating = false;
    }
    if (failed.length === 0) {
      const one = updated.length === 1 ? updated[0] : null;
      notify({
        id: CONNECTOR_UPDATES_NOTE,
        kind: 'success',
        title: one ? `${one.o.connector.name} updated` : `${updated.length} connectors updated`,
        message: one
          ? `Now ${one.version}.`
          : updated.map((u) => `${u.o.connector.name}: ${u.version}`).join('\n'),
        timeout: 8000,
      });
      return;
    }
    const lines = failed.map((f) => `${f.o.connector.name}: ${f.message}`);
    if (updated.length > 0) {
      lines.unshift(...updated.map((u) => `${u.o.connector.name}: updated to ${u.version}.`));
    }
    notify({
      id: CONNECTOR_UPDATES_NOTE,
      kind: 'warning',
      title:
        failed.length === 1
          ? `${failed[0].o.connector.name} couldn’t be updated`
          : `${failed.length} connectors couldn’t be updated`,
      message: lines.join('\n'),
      actions: [
        { label: 'Open Connectors', primary: true, run: deps.openConnectors },
        { label: 'Close', run: () => {} },
      ],
    });
  }

  return { offer, update };
}

const connectorUpdater = createConnectorUpdater({
  prefs: sharedMap('connector-updates', { key: 'rovik.connectorUpdates' }),
  runUpdate: (connector) => runSetupStep(setupCall(null), connector, 'update', () => {}),
  check: (connector) => checkConnector(connector),
  openConnectors: () => openAppSettings('connectors'),
});

/** From App.tsx, after the start's check: offers the newer versions it found. */
export const offerConnectorUpdates = (providers: Provider[], found: Record<string, CheckedState>) =>
  connectorUpdater.offer(providers, found);
