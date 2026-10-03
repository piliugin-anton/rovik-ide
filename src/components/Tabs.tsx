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
            classList={{ active: file.path === props.activePath(), deleted: !!file.deleted }}
            onClick={() => props.onSelect(file.path)}
            title={file.deleted ? `${file.path} (deleted from disk; save to restore)` : file.path}
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
