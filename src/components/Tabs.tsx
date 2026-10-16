import { Index, type Accessor } from 'solid-js';
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
      {/* By position, not by object: a tab's file is a new object on every
          write to it (the agent's too), and a new tab element under a click
          (select, close) lost the click. */}
      <Index each={props.files()}>
        {(file) => (
          <div
            class="tab"
            classList={{
              active: file().path === props.activePath(),
              deleted: !!file().deleted,
              readonly: !!file().readOnly,
            }}
            onClick={() => props.onSelect(file().path)}
            // Middle click closes, as in browsers and most editors. Pressing
            // the wheel would otherwise start autoscroll.
            onMouseDown={(e) => {
              if (e.button === 1) e.preventDefault();
            }}
            onAuxClick={(e) => {
              if (e.button !== 1) return;
              e.preventDefault();
              props.onClose(file().path);
            }}
            title={tabTitle(file())}
          >
            <span class="tab-name">
              {file().deleted ? `${file().name} [DELETED]` : file().name}
              {file().dirty ? ' •' : ''}
            </span>
            <span
              class="tab-close"
              onClick={(e) => {
                e.stopPropagation();
                props.onClose(file().path);
              }}
            >
              ×
            </span>
          </div>
        )}
      </Index>
    </div>
  );
}

function tabTitle(file: OpenFile): string {
  if (file.stash)
    return `${file.stash.file} — stashed preview (${file.stash.message ?? `stash@${file.stash.index}`}), read-only`;
  return file.deleted ? `${file.path} (deleted from disk; save to restore)` : file.path;
}
