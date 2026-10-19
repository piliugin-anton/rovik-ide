// network.ts
//
// Settings → Network (src-tauri/crates/rovik-core/src/net): how long
// connecting may take, the proxy the app's connections go through (none,
// the system's, or one set here, HTTP or SOCKS5), and whether cloud hosts'
// SSH connections and the agent's browsers (this computer's, the cloud
// hosts') go through it too. Kept by the backend, the custom proxy's
// password in the keychain.
import { invoke } from '@tauri-apps/api/core';
import type { StoreKind } from './hosts';

/** 'direct': no proxy, the first choice, even when the system has one set. */
export type ProxyMode = 'direct' | 'system' | 'custom';
export type ProxyKind = 'http' | 'socks5';
/** Which of the agent's browsers go through the proxy. */
export type BrowserPlaces = 'local' | 'cloud' | 'both';

export interface CustomProxy {
  kind: ProxyKind;
  /** Its name or address, without a scheme or a port. */
  host: string;
  port: number;
  /** Signs in with this name and the saved password; none: no sign-in. */
  username?: string;
}

export interface NetworkSettings {
  /** How long connecting may take, in seconds; none: the default. */
  timeoutSecs?: number;
  proxy: ProxyMode;
  /** Kept while another choice is used, for going back to it. */
  custom?: CustomProxy;
  /** Cloud hosts' SSH connections go through the proxy too: kept, unused, with no proxy. */
  cloudHosts: boolean;
  /** The agent's browser goes through the proxy too, where `browserPlaces` says. */
  browser: boolean;
  browserPlaces: BrowserPlaces;
}

export interface NetworkView {
  settings: NetworkSettings;
  /** A password for the custom proxy is saved. */
  passwordSaved: boolean;
  /** The proxy "System proxy" uses now, as `scheme://host:port`; none: directly. */
  system: { https?: string | null; http?: string | null };
  defaultTimeoutSecs: number;
  minTimeoutSecs: number;
  maxTimeoutSecs: number;
  /**
   * Where the proxy's password goes: the keychain, or the file encrypted in
   * its place, which is unlocked once a session (Cloud hosts' passphrase).
   */
  store?: StoreKind;
}

export function networkSettings(): Promise<NetworkView> {
  return invoke<NetworkView>('network_settings');
}

/**
 * Keeps `settings`, and the proxy's password: undefined leaves it as it
 * is, an empty one forgets it.
 */
export function saveNetwork(settings: NetworkSettings, password?: string): Promise<NetworkView> {
  return invoke<NetworkView>('network_save', { settings, password });
}

export const KIND_LABEL: Record<ProxyKind, string> = { http: 'HTTP', socks5: 'SOCKS5' };

export const PLACES_LABEL: Record<BrowserPlaces, string> = {
  local: 'Local only',
  cloud: 'Cloud only',
  both: 'Local + cloud',
};

/** The port a kind of proxy usually takes. */
export const USUAL_PORT: Record<ProxyKind, number> = { http: 3128, socks5: 1080 };

/** `host:port`, an IPv6 address in brackets. */
export function proxyAddress(proxy: Pick<CustomProxy, 'host' | 'port'>): string {
  const host =
    proxy.host.includes(':') && !proxy.host.startsWith('[') ? `[${proxy.host}]` : proxy.host;
  return `${host}:${proxy.port}`;
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

/** What "System proxy" uses now, in a few words. */
export function systemProxyLine(view: Pick<NetworkView, 'system'>): string {
  const { https, http } = view.system;
  if (!https && !http) return 'None is set: connections go directly.';
  if (https && http && https !== http) {
    return `${describeProxyUrl(https)} for HTTPS, ${describeProxyUrl(http)} for HTTP.`;
  }
  return `${describeProxyUrl((https ?? http)!)}.`;
}

/** One line for the Settings menu. */
export function networkSummary(view: NetworkView): string {
  const { settings } = view;
  const direct = settings.proxy === 'direct';
  const custom = settings.proxy === 'custom' ? settings.custom : undefined;
  let line: string;
  if (direct) {
    line = 'No proxy: connections go directly';
  } else if (custom) {
    line = `Custom ${KIND_LABEL[custom.kind]} proxy: ${proxyAddress(custom)}`;
  } else {
    const { https, http } = view.system;
    const shown = https ?? http;
    line = shown
      ? `System proxy: ${describeProxyUrl(shown)}`
      : 'System proxy: none set, connections go directly';
  }
  const parts = [line];
  // What goes through the proxy besides, when there's one.
  const anyProxy = !direct && Boolean(custom || view.system.https || view.system.http);
  if (settings.cloudHosts && anyProxy) parts.push('also for cloud hosts');
  if (settings.browser && anyProxy) {
    parts.push(`the browser too (${PLACES_LABEL[settings.browserPlaces].toLowerCase()})`);
  }
  if (settings.timeoutSecs !== undefined && settings.timeoutSecs !== view.defaultTimeoutSecs) {
    parts.push(`${settings.timeoutSecs} s to connect`);
  }
  return parts.join(' · ');
}
