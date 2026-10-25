// BrowserView.tsx
//
// A browser tab's page (lib/browserTabs.ts): an address bar, with Reload
// and "Open in your browser", over the page in a frame. The frame is
// sandboxed: the page runs its scripts in its own origin (its cookies and
// storage), submits forms, shows dialogs and opens windows (which open in
// the default browser: src-tauri/src/lib.rs), but can't navigate the app
// away, as a page that breaks out of frames would. A page framed through
// a tab port says where it is as it navigates (its script, rovik-client's
// remote/tab.js): the address bar follows it, unless the user is typing
// there, and Reload and "Open in your browser" take it where it is. A tab
// keeps its frame while another one is shown (it's only hidden), so its
// page keeps its state; navigating or reloading gives the tab a new
// object, and App a new frame for it.
import { createMemo, createSignal, onCleanup, onMount, Show } from 'solid-js';
import { invoke } from '@tauri-apps/api/core';
import { addressFrom, pageAddress, reportedAddress } from '../lib/browserTabs';
import type { BrowserTab } from '../lib/editorTabs';

/** A web page's globe: a browser tab's, in the tab strip and elsewhere. */
export function GlobeIcon(props: { class?: string }) {
  return (
    <svg class={props.class} viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
      <circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" stroke-width="1.2" />
      <path
        d="M2 8h12M8 2c1.8 1.7 2.7 3.7 2.7 6s-.9 4.3-2.7 6M8 2C6.2 3.7 5.3 5.7 5.3 8s.9 4.3 2.7 6"
        fill="none"
        stroke="currentColor"
        stroke-width="1.2"
      />
    </svg>
  );
}

/** What a page in a browser tab may do: all but navigate the app away. */
export const FRAME_SANDBOX =
  'allow-scripts allow-same-origin allow-forms allow-modals allow-popups ' +
  'allow-popups-to-escape-sandbox';

export default function BrowserView(props: {
  tab: BrowserTab;
  /** It's the tab shown. */
  active: boolean;
  /** Where its frame loads the page (lib/browserTabs.ts: frameUrl). */
  frame: string;
  /** The address bar asks for `address` (a valid one). */
  onNavigate: (address: string) => void;
  /** Reload asks for the page again, at `address`, where it is now. */
  onReload: (address: string) => void;
}) {
  // Setting the frame's src again, even to the same address, reloads it.
  const frame = createMemo(() => props.frame);
  let frameEl: HTMLIFrameElement | undefined;
  // Where the page is, as it last said; where it loaded until it says.
  const [current, setCurrent] = createSignal(props.tab.address);
  const [typed, setTyped] = createSignal(props.tab.address);
  const [refused, setRefused] = createSignal(false);

  // Only this tab's frame is heard, and only of its own origin's pages.
  onMount(() => {
    const heard = (e: MessageEvent) => {
      if (!frameEl || e.source !== frameEl.contentWindow) return;
      const href = reportedAddress(e.data);
      const address = href ? pageAddress(href, frame(), props.tab.address) : null;
      // pageAddress has read href: a URL at the frame's origin.
      if (!href || !address || new URL(href).origin !== e.origin) return;
      // What the user is typing stays.
      if (typed() === current()) setTyped(address);
      setCurrent(address);
    };
    window.addEventListener('message', heard);
    onCleanup(() => window.removeEventListener('message', heard));
  });

  function go(e: SubmitEvent) {
    e.preventDefault();
    const address = addressFrom(typed(), window.location.origin);
    if (address) props.onNavigate(address);
    else setRefused(true);
  }

  function openOutside() {
    invoke('open_url', { url: current() }).catch((err) =>
      console.error('Open in the browser:', err)
    );
  }

  return (
    <div class="browser-view" classList={{ active: props.active }}>
      <form class="browser-bar" onSubmit={go}>
        <button
          type="button"
          class="browser-bar-button"
          onClick={() => props.onReload(current())}
          title="Reload"
          aria-label="Reload"
        >
          <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
            <path
              d="M13 8a5 5 0 1 1-1.46-3.54M13 2.5v3h-3"
              fill="none"
              stroke="currentColor"
              stroke-width="1.4"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        </button>
        <input
          class="browser-address"
          classList={{ refused: refused() }}
          value={typed()}
          onInput={(e) => {
            setTyped(e.currentTarget.value);
            setRefused(false);
          }}
          onKeyDown={(e) => {
            if (e.key !== 'Escape') return;
            setTyped(current());
            setRefused(false);
          }}
          spellcheck={false}
          autocomplete="off"
          aria-label="Address"
          aria-invalid={refused()}
        />
        <Show when={refused()}>
          <span class="browser-bar-note" role="alert">
            Only http and https pages open here
          </span>
        </Show>
        <button
          type="button"
          class="browser-bar-button"
          onClick={openOutside}
          title="Open in your browser"
          aria-label="Open in your browser"
        >
          <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
            <path
              d="M9 2.5h4.5V7M13.5 2.5 7.5 8.5M12 10v2.5a1 1 0 0 1-1 1H3.5a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1H6"
              fill="none"
              stroke="currentColor"
              stroke-width="1.3"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        </button>
      </form>
      <iframe
        ref={frameEl}
        class="browser-frame"
        src={frame()}
        title={current()}
        sandbox={FRAME_SANDBOX}
        allow="clipboard-write"
      />
    </div>
  );
}
