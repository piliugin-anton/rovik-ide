import { createMemo, Index, Show, type Accessor, type JSX } from 'solid-js';
import { pageLabel } from '../lib/browserTabs';
import { GlobeIcon } from './BrowserView';
import { isBrowserTab, tabHints, type EditorTab } from '../lib/editorTabs';

interface TabsProps {
  /** Files and browser tabs, in order (lib/editorTabs.ts). */
  tabs: Accessor<EditorTab[]>;
  activePath: Accessor<string | null>;
  /**
   * The window's folders, by path and stored name: two tabs of one name say
   * which folder each is in (C03). A project passes its one folder.
   */
  folders: Accessor<readonly { path: string; name: string }[]>;
  onSelect: (path: string) => void;
  onClose: (path: string) => void;
}

export default function Tabs(props: TabsProps) {
  /** Where a tab whose name another tab has is: "api", "a/src" (C03). */
  const hints = createMemo(() => tabHints(props.tabs(), props.folders()));
  const hintOf = (tab: EditorTab) => hints().get(tab.path);

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
              <span class="tab-name">{tabName(tab(), hintOf(tab()))}</span>
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

function tabName(tab: EditorTab, hint: string | undefined): JSX.Element {
  if (isBrowserTab(tab)) return pageLabel(tab.address);
  return (
    <>
      {tab.deleted ? `${tab.name} [DELETED]` : tab.name}
      {tab.dirty ? ' •' : ''}
      <Show when={hint}>{(where) => <span class="tab-hint">· {where()}</span>}</Show>
    </>
  );
}

function tabTitle(tab: EditorTab): string {
  if (isBrowserTab(tab)) return tab.address;
  if (tab.stash)
    return `${tab.stash.file} — stashed preview (${tab.stash.message ?? `stash@${tab.stash.index}`}), read-only`;
  return tab.deleted ? `${tab.path} (deleted from disk; save to restore)` : tab.path;
}
