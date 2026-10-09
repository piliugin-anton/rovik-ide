import { For, type Accessor } from 'solid-js';
import type { OpenFile } from '../types';

interface TabsProps {
  files: Accessor<OpenFile[]>;
  activePath: Accessor<string | null>;
  onSelect: (path: string) => void;
  onClose: (path: string) => void;
}

export default function Tabs(props: TabsProps) {
  return (
    <div class="tabs">
      <For each={props.files()}>
        {(file) => (
          <div
            class="tab"
            classList={{
              active: file.path === props.activePath(),
              deleted: !!file.deleted,
              readonly: !!file.readOnly,
            }}
            onClick={() => props.onSelect(file.path)}
            // Middle click closes, as in browsers and most editors. Pressing
            // the wheel would otherwise start autoscroll.
            onMouseDown={(e) => {
              if (e.button === 1) e.preventDefault();
            }}
            onAuxClick={(e) => {
              if (e.button !== 1) return;
              e.preventDefault();
              props.onClose(file.path);
            }}
            title={
              file.stash
                ? `${file.stash.file} — stashed preview (${file.stash.message ?? `stash@${file.stash.index}`}), read-only`
                : file.deleted
                  ? `${file.path} (deleted from disk; save to restore)`
                  : file.path
            }
          >
            <span class="tab-name">
              {file.deleted ? `${file.name} [DELETED]` : file.name}
              {file.dirty ? ' •' : ''}
            </span>
            <span
              class="tab-close"
              onClick={(e) => {
                e.stopPropagation();
                props.onClose(file.path);
              }}
            >
              ×
            </span>
          </div>
        )}
      </For>
    </div>
  );
}
