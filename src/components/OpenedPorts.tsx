// OpenedPorts.tsx
//
// The ports a remote chat's servers opened on its team server that a page
// was opened from (D20), in a row of their own under the chat's title row:
// "Opened ports: 3000 5173". Each is the server's own port; its button
// copies the address on this computer that leads to it, localhost and the
// port forwarded here, which differs where the server's port was taken on
// this computer. Hidden while the chat has none (lib/cloud.ts keeps them).
import { createSignal, For, Show } from 'solid-js';
import { cloud, type ForwardedPort } from '../lib/cloud';

/** The address on this computer that leads to the server's port. */
export const localAddress = (port: ForwardedPort) => `localhost:${port.localPort}`;

export default function OpenedPorts(props: { dialogId: string | null }) {
  const ports = () => (props.dialogId ? (cloud.ports[props.dialogId] ?? []) : []);
  return (
    <Show when={ports().length > 0}>
      <div class="opened-ports">
        <span class="opened-ports-label">Opened ports:</span>
        <For each={ports()}>{(port) => <OpenedPort port={port} />}</For>
      </div>
    </Show>
  );
}

function OpenedPort(props: { port: ForwardedPort }) {
  const [copied, setCopied] = createSignal(false);
  const address = () => localAddress(props.port);
  const title = () =>
    props.port.localPort === props.port.port
      ? `The chat's server listens on port ${props.port.port} on the server: ${address()} on this computer leads to it.`
      : `The chat's server listens on port ${props.port.port} on the server: ${address()} on this computer leads to it, as its own port was taken here.`;

  async function copy() {
    try {
      await navigator.clipboard.writeText(address());
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1200);
    } catch (err) {
      console.error('Copy failed:', err);
    }
  }

  return (
    <span class="opened-port" title={title()}>
      <span class="opened-port-number">{props.port.port}</span>
      <button
        type="button"
        class="opened-port-copy"
        classList={{ copied: copied() }}
        onClick={() => void copy()}
        title={copied() ? 'Copied' : `Copy ${address()}`}
        aria-label={`Copy ${address()}`}
      >
        <Show
          when={copied()}
          fallback={
            <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
              <rect
                x="5.5"
                y="5.5"
                width="8"
                height="8"
                rx="1.5"
                fill="none"
                stroke="currentColor"
                stroke-width="1.3"
              />
              <path
                d="M10.5 3.5v-.5a1.5 1.5 0 0 0-1.5-1.5H4A1.5 1.5 0 0 0 2.5 3v5A1.5 1.5 0 0 0 4 9.5h.5"
                fill="none"
                stroke="currentColor"
                stroke-width="1.3"
              />
            </svg>
          }
        >
          <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
            <path
              d="m3.5 8.5 3 3 6-7"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        </Show>
      </button>
    </span>
  );
}
