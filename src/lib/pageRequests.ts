import { Channel, invoke } from '@tauri-apps/api/core';
import { createEffect, createRoot, onCleanup } from 'solid-js';
import { desktopScope } from './desktopScope';
import { handleBackendCloud } from './cloud';
import { notify } from './notifications';
import { pageCallbacks, type PageRequest } from './pageCallbacks';

/** Starts the private callback once the window can create browser tabs. */
export function startPageRequests(openTab: (address: string) => void): () => void {
  return createRoot((dispose) => {
    const callbacks = pageCallbacks({
      scope: desktopScope,
      subscribe: (scope, handler) => {
        const onEvent = new Channel<PageRequest>();
        onEvent.onmessage = handler;
        return invoke<string>('desktop_pages_subscribe', { scope, onEvent });
      },
      unsubscribe: (id) => invoke('desktop_pages_unsubscribe', { id }),
      applyPorts: ({ dialogId, ports }) => handleBackendCloud({ type: 'ports', dialogId, ports }),
      openTab,
      openExternal: (url) => invoke('open_url', { url }),
      failed: (error) =>
        notify({
          id: 'page-callback',
          kind: 'error',
          title: 'The page could not open',
          message: String(error),
        }),
    });
    createEffect(() => callbacks.refresh());
    onCleanup(callbacks.stop);
    return dispose;
  });
}
