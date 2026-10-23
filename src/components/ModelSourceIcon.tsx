// ModelSourceIcon.tsx
//
// Where a model comes from, at a glance: a model provider (an API
// endpoint: a cloud) or a connector (a harness on this computer: a plug),
// beside the model's name in the picker and the pickers of the settings.
import type { JSX } from 'solid-js';

export type ModelSource = 'provider' | 'connector';

export function sourceTitle(source: ModelSource): string {
  return source === 'connector'
    ? 'A connector: a coding harness installed where the chat runs'
    : 'A model provider: an API endpoint';
}

export default function ModelSourceIcon(props: {
  source: ModelSource;
  size?: number;
  class?: string;
}): JSX.Element {
  const size = () => props.size ?? 12;
  return (
    <svg
      class={`model-source-icon ${props.source} ${props.class ?? ''}`}
      width={size()}
      height={size()}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      role="img"
      aria-label={sourceTitle(props.source)}
    >
      {props.source === 'connector' ? (
        <>
          {/* A plug: two prongs, the body, the cord. */}
          <path d="M5.5 1.5v3M10.5 1.5v3" />
          <path d="M3.5 4.5h9v2a4.5 4.5 0 0 1-9 0v-2z" />
          <path d="M8 11v3.5" />
        </>
      ) : (
        <>
          {/* A cloud. */}
          <path d="M4.5 12.5a3 3 0 0 1-.4-5.98A4 4 0 0 1 11.8 6a2.75 2.75 0 0 1 .2 5.5v1H4.5z" />
        </>
      )}
    </svg>
  );
}
