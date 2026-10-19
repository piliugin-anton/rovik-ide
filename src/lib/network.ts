// network.ts
//
// Settings → Network (src-tauri/crates/rovik-core/src/net): how long
// connecting may take, the proxies Rovik can use, each typed once, and
// where each kind of connection goes: from this computer, its requests to
// model providers and its downloads, its SSH connections to cloud hosts
// and the agent's browser; from cloud hosts, their requests and their
// browsers. Kept by the backend, the proxies' passwords in the keychain.
import { invoke } from '@tauri-apps/api/core';
import type { StoreKind } from './hosts';

export type ProxyKind = 'http' | 'socks5';

/** A saved proxy. Its password is in the keychain, never here. */
export interface SavedProxy {
  /** What routes name it by: letters, digits, `-` and `_`, unique. */
  id: string;
  kind: ProxyKind;
  /** Its name or address, without a scheme or a port. */
  host: string;
  port: number;
  /** Signs in with this name and its saved password; none: no sign-in. */
  username?: string;
  /** A password for it is saved: the backend says, and ignores it sent. */
  passwordSaved?: boolean;
}

/**
 * Where one kind of connection goes: `auto`, this computer's proxy while
 * it has one set, else none (model providers here only); `own`, the
 * browser's own settings, or the host's; `direct`, no proxy, even when the
 * computer has one set; `system`, this computer's proxy; or a saved proxy.
 */
export type Route = 'auto' | 'own' | 'direct' | 'system' | { proxy: string };

/** A route other than a saved proxy. */
export type BuiltinRoute = Exclude<Route, { proxy: string }>;

/** Each kind of connection, and where it goes. */
export interface Routes {
  /** This computer's requests to model providers, and its downloads. */
  models: Route;
  /** SSH from this computer to cloud hosts. */
  ssh: Route;
  /** The agent's browser on this computer. */
  browser: Route;
  /** Cloud hosts' requests to model providers, and their downloads. */
  hostModels: Route;
  /** The agent's browser on cloud hosts. */
  hostBrowser: Route;
}

export type RouteRow = keyof Routes;

export interface NetworkSettings {
  /** How long connecting may take, in seconds; none: the default. */
  timeoutSecs?: number;
  proxies: SavedProxy[];
  routes: Routes;
}

export interface NetworkView {
  settings: NetworkSettings;
  /** This computer's proxy, "System proxy", as `scheme://host:port`; none: directly. */
  system: { https?: string | null; http?: string | null };
  defaultTimeoutSecs: number;
  minTimeoutSecs: number;
  maxTimeoutSecs: number;
  /**
   * Where the proxies' passwords go: the keychain, or the file encrypted in
   * its place, which is unlocked once a session (Cloud hosts' passphrase).
   */
  store?: StoreKind;
}

export function networkSettings(): Promise<NetworkView> {
  return invoke<NetworkView>('network_settings');
}

/**
 * Keeps `settings`, and new passwords for the proxies `passwords` names: an
 * empty one forgets it, a proxy not named keeps its own. A proxy left out of
 * `settings.proxies` is removed, its password with it. Refused, with why,
 * when the settings can't be used or a password can't be stored; nothing
 * is saved then.
 */
export function saveNetwork(
  settings: NetworkSettings,
  passwords?: Record<string, string>
): Promise<NetworkView> {
  return invoke<NetworkView>('network_save', { settings, passwords });
}

export const KIND_LABEL: Record<ProxyKind, string> = { http: 'HTTP', socks5: 'SOCKS5' };

/** The port a kind of proxy usually takes. */
export const USUAL_PORT: Record<ProxyKind, number> = { http: 3128, socks5: 1080 };

/** What each kind of connection goes by until another route is chosen. */
export const DEFAULT_ROUTES: Routes = {
  models: 'auto',
  ssh: 'direct',
  browser: 'own',
  hostModels: 'own',
  hostBrowser: 'own',
};

export interface RowInfo {
  row: RouteRow;
  /** This computer's, or the cloud hosts'. */
  place: 'here' | 'cloud';
  /** What the page calls it, under its place. */
  label: string;
  /** Its menu's name for assistive technology, the place in it. */
  ariaLabel: string;
  /** What the Settings menu's line calls it. */
  name: string;
}

/** The kinds of connection, in the page's order. */
export const ROWS: RowInfo[] = [
  {
    row: 'models',
    place: 'here',
    label: 'Model providers and downloads',
    ariaLabel: 'Model providers and downloads on this computer',
    name: 'model providers',
  },
  {
    row: 'ssh',
    place: 'here',
    label: 'SSH to cloud hosts',
    ariaLabel: 'SSH to cloud hosts',
    name: 'SSH to cloud hosts',
  },
  {
    row: 'browser',
    place: 'here',
    label: 'Agent’s browser',
    ariaLabel: 'Agent’s browser on this computer',
    name: 'the browser',
  },
  {
    row: 'hostModels',
    place: 'cloud',
    label: 'Model providers and downloads',
    ariaLabel: 'Model providers and downloads on cloud hosts',
    name: 'cloud hosts’ model providers',
  },
  {
    row: 'hostBrowser',
    place: 'cloud',
    label: 'Agent’s browser',
    ariaLabel: 'Agent’s browser on cloud hosts',
    name: 'cloud hosts’ browser',
  },
];

/** The routes other than a saved proxy each kind takes, in its menu's order. */
export const ROW_TAKES: Record<RouteRow, BuiltinRoute[]> = {
  models: ['auto', 'direct', 'system'],
  ssh: ['direct', 'system'],
  browser: ['own', 'direct', 'system'],
  hostModels: ['own', 'direct'],
  hostBrowser: ['own', 'direct'],
};

/** A route as its menu's option value: `auto`, `own`, `direct`, `system` or `proxy:<id>`. */
export function routeValue(route: Route): string {
  return typeof route === 'string' ? route : `proxy:${route.proxy}`;
}

/** The route an option value stands for. */
export function routeOf(value: string): Route {
  return value.startsWith('proxy:') ? { proxy: value.slice('proxy:'.length) } : (value as Route);
}

/** The saved proxy a route names, if it names one. */
export function routeProxy(route: Route): string | undefined {
  return typeof route === 'string' ? undefined : route.proxy;
}

/** `host:port`, an IPv6 address in brackets. */
export function proxyAddress(proxy: Pick<SavedProxy, 'host' | 'port'>): string {
  const host =
    proxy.host.includes(':') && !proxy.host.startsWith('[') ? `[${proxy.host}]` : proxy.host;
  return `${host}:${proxy.port}`;
}

/** A saved proxy as the menus say it: `HTTP proxy.corp:3128 (alice)`. */
export function proxyLabel(proxy: SavedProxy): string {
  const user = proxy.username ? ` (${proxy.username})` : '';
  return `${KIND_LABEL[proxy.kind]} ${proxyAddress(proxy)}${user}`;
}

/** This computer has a proxy set, as "System proxy" uses it. */
export function systemHasProxy(view: Pick<NetworkView, 'system'>): boolean {
  return Boolean(view.system.https || view.system.http);
}

/** What an option of a kind's menu says. */
export function routeLabel(view: NetworkView, row: RouteRow, route: Route): string {
  if (typeof route !== 'string') {
    const proxy = view.settings.proxies.find((p) => p.id === route.proxy);
    return proxy ? proxyLabel(proxy) : 'A proxy no longer saved';
  }
  switch (route) {
    case 'auto':
      return systemHasProxy(view) ? 'Automatic (system proxy)' : 'Automatic (no proxy)';
    case 'own':
      return row === 'hostModels' ? 'The host’s own settings' : 'Its own settings';
    case 'direct':
      return 'No proxy';
    case 'system':
      return 'System proxy';
  }
}

/** A kind's menu: the routes it takes, then the saved proxies. */
export function rowOptions(view: NetworkView, row: RouteRow): { value: string; label: string }[] {
  const routes: Route[] = [
    ...ROW_TAKES[row],
    ...view.settings.proxies.map((p) => ({ proxy: p.id })),
  ];
  return routes.map((route) => ({ value: routeValue(route), label: routeLabel(view, row, route) }));
}

/** The kinds of connection that go through the saved proxy `id`. */
export function proxyUses(settings: NetworkSettings, id: string): RouteRow[] {
  return ROWS.map((r) => r.row).filter((row) => routeProxy(settings.routes[row]) === id);
}

/** A fresh id for a new proxy, none of `existing`: `proxy-` and 8 letters or digits. */
export function newProxyId(existing: string[]): string {
  const taken = new Set(existing);
  for (;;) {
    let tail = '';
    while (tail.length < 8) tail += Math.random().toString(36).slice(2);
    const id = `proxy-${tail.slice(0, 8)}`;
    if (!taken.has(id)) return id;
  }
}

/**
 * Whether a kind's connections go through a proxy now: a saved one, or
 * this computer's ("System proxy", or automatically) while it has one set.
 */
export function throughProxy(view: NetworkView, row: RouteRow): boolean {
  const route = view.settings.routes[row];
  if (typeof route !== 'string') return view.settings.proxies.some((p) => p.id === route.proxy);
  return (route === 'system' || route === 'auto') && systemHasProxy(view);
}

/** A port as typed: a whole number from 1 to 65535, else undefined. */
export function portOf(text: string): number | undefined {
  const value = Number(text.trim());
  return Number.isInteger(value) && value >= 1 && value <= 65535 ? value : undefined;
}

/**
 * What was typed or pasted in the host field, taken apart: a proxy's URL
 * ("http://proxy.example.com:3128", "socks5://10.0.0.2:1080") gives its
 * kind and port too. Anything else is the host as it is.
 */
export function splitProxyAddress(text: string): {
  host: string;
  port?: number;
  kind?: ProxyKind;
} {
  let rest = text.trim();
  let kind: ProxyKind | undefined;
  const scheme = /^([a-z0-9]+):\/\//i.exec(rest);
  if (scheme) {
    const name = scheme[1].toLowerCase();
    if (name === 'http' || name === 'https') kind = 'http';
    else if (name.startsWith('socks')) kind = 'socks5';
    else return { host: rest };
    rest = rest.slice(scheme[0].length);
  }
  // A path or a trailing slash isn't the proxy's.
  rest = rest.replace(/\/.*$/, '');
  const bracketed = /^\[([^\]]+)\](?::(\d+))?$/.exec(rest);
  if (bracketed) {
    return { host: bracketed[1], port: bracketed[2] ? portOf(bracketed[2]) : undefined, kind };
  }
  const hostPort = /^([^:@\s]+):(\d+)$/.exec(rest);
  if (hostPort) return { host: hostPort[1], port: portOf(hostPort[2]), kind };
  return { host: rest, kind };
}

/** The system's proxy, as its line says it: `proxy.example.com:3128 (HTTP)`. */
export function describeProxyUrl(url: string): string {
  const match = /^([a-z0-9]+):\/\/(.+?)\/?$/i.exec(url);
  if (!match) return url;
  const scheme = match[1].toLowerCase();
  const kind = scheme.startsWith('socks') ? 'SOCKS5' : scheme === 'https' ? 'HTTPS' : 'HTTP';
  return `${match[2]} (${kind})`;
}

/** This computer's proxy in a few words: `proxy.corp:3128 (HTTP)`, or `none set`. */
export function systemProxyLine(view: Pick<NetworkView, 'system'>): string {
  const { https, http } = view.system;
  if (!https && !http) return 'none set';
  if (https && http && https !== http) {
    return `${describeProxyUrl(https)} for HTTPS, ${describeProxyUrl(http)} for HTTP`;
  }
  return describeProxyUrl((https ?? http)!);
}

/** One line for the Settings menu: which kinds of connection go through a proxy. */
export function networkSummary(view: NetworkView): string {
  const through = ROWS.filter((r) => throughProxy(view, r.row)).map((r) => r.name);
  const parts = [
    through.length ? `Through a proxy: ${through.join(', ')}` : 'No proxy: connections go directly',
  ];
  const { timeoutSecs } = view.settings;
  if (timeoutSecs !== undefined && timeoutSecs !== view.defaultTimeoutSecs) {
    parts.push(`${timeoutSecs} s to connect`);
  }
  return parts.join(' · ');
}
