// browserTabs.ts
//
// What a browser tab of the editor shows (BrowserView.tsx): a web page, in
// a frame. A page a remote chat's command opens on its team server (D20)
// opens in one as the app asks (`open-page`, src-tauri/src/lib.rs), and
// "Opened ports" opens one again (OpenedPorts.tsx); App has the tabs
// (lib/editorTabs.ts). Such a page is framed through its forward's tab
// port, which lifts what keeps a page out of a frame (rovik-client's
// remote/tab_port.rs), while its address bar shows it at the port
// forwarded here, where the default browser opens it.
import { listen } from '@tauri-apps/api/event';
import type { ForwardedPort } from './cloud';

/** A scheme at the start of an address (`https:`). */
const SCHEME = /^([a-z][a-z0-9+.-]*):/i;

/** The names of this computer's loopback a forwarded port listens on. */
const LOOPBACK = new Set(['localhost', '127.0.0.1', '[::1]']);

/**
 * The address `text` names for a browser tab: an http or https one (a
 * bare host, `localhost:5173`, gets http://), never the app's own page
 * (`appOrigin`). Null when it names none.
 */
export function addressFrom(text: string, appOrigin: string): string | null {
  const trimmed = text.trim();
  if (!trimmed) return null;
  const scheme = SCHEME.exec(trimmed);
  // A scheme, unless what it names is a host and what follows its port.
  const named =
    scheme && !/^\d/.test(trimmed.slice(scheme[0].length)) ? trimmed : `http://${trimmed}`;
  let url: URL;
  try {
    url = new URL(named);
  } catch {
    return null;
  }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') return null;
  if (url.origin === appOrigin) return null;
  return url.href;
}

/**
 * Where a browser tab's frame loads `address`: through the tab port of the
 * forward whose port here it names (lib/cloud.ts), else at the address.
 */
export function frameUrl(address: string, ports: readonly ForwardedPort[]): string {
  let url: URL;
  try {
    url = new URL(address);
  } catch {
    return address;
  }
  if (url.protocol !== 'http:' || !LOOPBACK.has(url.hostname)) return address;
  const port = Number(url.port || 80);
  const tabPort = ports.find((p) => p.localPort === port && p.tabPort)?.tabPort;
  if (!tabPort) return address;
  url.port = String(tabPort);
  return url.href;
}

/** A browser tab's name in the tab strip: its page's host and port. */
export function pageLabel(address: string): string {
  try {
    return new URL(address).host || address;
  } catch {
    return address;
  }
}

type PageOpener = (address: string) => void;

let opener: PageOpener | null = null;

/** App's: it has the tabs. */
export function setPageOpener(next: PageOpener | null): void {
  opener = next;
}

/** Opens `address` in a browser tab: the one showing it already, else a new one. */
export function openPage(address: string): void {
  opener?.(address);
}

/**
 * Calls `handler` with each page the app asks a browser tab to open (a
 * remote chat's command's, D20), until the function it returns is called.
 */
export function onPageWanted(handler: (address: string) => void): () => void {
  const listening = listen<string>('open-page', (e) => handler(e.payload));
  return () => void listening.then((stop) => stop());
}
