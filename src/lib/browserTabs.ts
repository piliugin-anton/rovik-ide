// browserTabs.ts
//
// What a browser tab of the editor shows (BrowserView.tsx): a web page, in
// a frame. A page a remote chat's command opens on its team server (D20)
// opens in one as the app asks (`open-page`, src-tauri/src/lib.rs), and
// "Opened ports" opens one again (OpenedPorts.tsx); App has the tabs
// (lib/editorTabs.ts). Such a page is framed through its forward's tab
// port, which lifts what keeps a page out of a frame (rovik-client's
// remote/tab_port.rs), while its address bar shows it at the port
// forwarded here, where the default browser opens it. The tab port's
// script in the page tells the tab where the page is as it navigates, for
// the address bar to follow it (`reportedAddress`, `pageAddress`). The tab
// closes once its port isn't forwarded any more: nothing serves its page
// then (`closeTabsOfClosedPorts`).
import { createEffect, on } from 'solid-js';
import { listen } from '@tauri-apps/api/event';
import type { ForwardedPort } from './cloud';
import type { BrowserTab } from './editorTabs';

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
 * `address` read as an http page at this computer's loopback, with the
 * port it's at there: a port forwarded here, if one is. Null for any other
 * address.
 */
function loopbackPage(address: string): { url: URL; port: number } | null {
  let url: URL;
  try {
    url = new URL(address);
  } catch {
    return null;
  }
  if (url.protocol !== 'http:' || !LOOPBACK.has(url.hostname)) return null;
  return { url, port: Number(url.port || 80) };
}

/**
 * Where a browser tab's frame loads `address`: through the tab port of the
 * forward whose port here it names (lib/cloud.ts), else at the address.
 */
export function frameUrl(address: string, ports: readonly ForwardedPort[]): string {
  const page = loopbackPage(address);
  const tabPort = page && ports.find((p) => p.localPort === page.port && p.tabPort)?.tabPort;
  if (!page || !tabPort) return address;
  page.url.port = String(tabPort);
  return page.url.href;
}

/**
 * Closes each browser tab (`close`) whose page is at a port forwarded here
 * once that port isn't forwarded any more (`ports`: every chat's). Nothing
 * serves the page then: its server stopped listening on the host (its
 * command ended, timed out or was stopped), its chat left the host, or the
 * host went. A port another chat still has keeps its tabs. Call once,
 * within a component.
 */
export function closeTabsOfClosedPorts(
  tabs: () => readonly BrowserTab[],
  ports: () => readonly ForwardedPort[],
  close: (path: string) => void
): void {
  createEffect(
    on(
      () => new Set(ports().map((p) => p.localPort)),
      (now, before) => {
        // The ports as it starts: none of them has closed.
        if (!before) return;
        for (const tab of tabs()) {
          const port = loopbackPage(tab.address)?.port;
          if (port !== undefined && before.has(port) && !now.has(port)) close(tab.path);
        }
      }
    )
  );
}

/**
 * The address a page a browser tab frames says it's at (`data`, the
 * message the tab port's script posts as the page navigates: rovik-client's
 * remote/tab.js). Null for any other message.
 */
export function reportedAddress(data: unknown): string | null {
  if (typeof data !== 'object' || data === null) return null;
  const { type, href } = data as { type?: unknown; href?: unknown };
  return type === 'rovik-tab-location' && typeof href === 'string' ? href : null;
}

/**
 * Where a browser tab's page is, for its address bar, when its frame
 * (loaded from `frame`) is at `href`: the same path at `address`'s
 * origin, the port forwarded here rather than the tab port. Null when
 * `href` is at another origin than the frame's.
 */
export function pageAddress(href: string, frame: string, address: string): string | null {
  try {
    const at = new URL(href);
    if (at.origin !== new URL(frame).origin) return null;
    return new URL(at.pathname + at.search + at.hash, new URL(address).origin).href;
  } catch {
    return null;
  }
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
