import { Index, Show, type Accessor } from 'solid-js';
import { pageLabel } from '../lib/browserTabs';
import { GlobeIcon } from './BrowserView';
import { isBrowserTab, type EditorTab } from '../lib/editorTabs';

interface TabsProps {
  /** Files and browser tabs, in order (lib/editorTabs.ts). */
  tabs: Accessor<EditorTab[]>;
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
      <Index each={props.tabs()}>
        {(tab) => {
          const file = () => {
            const t = tab();
            return isBrowserTab(t) ? null : t;
          };
          return (
            <div
              class="tab"
              classList={{
                active: tab().path === props.activePath(),
                deleted: !!file()?.deleted,
                readonly: !!file()?.readOnly,
                browser: isBrowserTab(tab()),
              }}
              onClick={() => props.onSelect(tab().path)}
              // Middle click closes, as in browsers and most editors. Pressing
              // the wheel would otherwise start autoscroll.
              onMouseDown={(e) => {
                if (e.button === 1) e.preventDefault();
              }}
              onAuxClick={(e) => {
                if (e.button !== 1) return;
                e.preventDefault();
                props.onClose(tab().path);
              }}
              title={tabTitle(tab())}
            >
              <Show when={isBrowserTab(tab())}>
                <GlobeIcon class="tab-icon" />
              </Show>
              <span class="tab-name">{tabName(tab())}</span>
              <span
                class="tab-close"
                onClick={(e) => {
                  e.stopPropagation();
                  props.onClose(tab().path);
                }}
              >
                ×
              </span>
            </div>
          );
        }}
      </Index>
    </div>
  );
}

function tabName(tab: EditorTab): string {
  if (isBrowserTab(tab)) return pageLabel(tab.address);
  return `${tab.deleted ? `${tab.name} [DELETED]` : tab.name}${tab.dirty ? ' •' : ''}`;
}

function tabTitle(tab: EditorTab): string {
  if (isBrowserTab(tab)) return tab.address;
  if (tab.stash)
    return `${tab.stash.file} — stashed preview (${tab.stash.message ?? `stash@${tab.stash.index}`}), read-only`;
  return tab.deleted ? `${tab.path} (deleted from disk; save to restore)` : tab.path;
}
