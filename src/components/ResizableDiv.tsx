import { createSignal, onCleanup, JSX, Show } from 'solid-js';

interface ResizableProps {
  children: JSX.Element;
  class?: string;
  initialWidth?: number;
  minWidth?: number;
  maxWidth?: number;
  /** The panel fills the window; the right pane is hidden by `.sidebar-expanded`. */
  expanded?: boolean;
  onExpandedChange?: (expanded: boolean) => void;
  /** Whether the floating expand toggle shows: App's to say (on the Chat tab). */
  showFloatingButton: boolean;
}

// The left panel is chrome: tabs, pickers, rows, toolbars, chat messages.
// Text selection stays working only where it is needed for editing or
// copying: inputs (including the file-tree rename/create input), the
// composer, settings and error text. Mirrors the `.sidebar` user-select
// opt-ins in styles.css — keep the two in sync.
const SELECTABLE =
  "input, textarea, [contenteditable]:not([contenteditable='false']), " +
  '.modal-body, .chat-history-error, ' +
  '.git-inline-error, .git-panel-error';

function isSelectable(target: EventTarget | null): boolean {
  // Some WebViews report a text node as the event target.
  const el =
    target instanceof Element ? target : target instanceof Node ? target.parentElement : null;
  if (el?.closest(SELECTABLE)) return true;
  // ... and some report document/window for a selection that lives inside
  // an input (e.g. the file-tree rename/create input). Only then fall back
  // to the focused element, so an editing input is never blocked while
  // chrome around it stays unselectable.
  if (el === null) {
    const active = document.activeElement;
    return active instanceof Element && !!active.closest(SELECTABLE);
  }
  return false;
}

export default function Resizable(props: ResizableProps) {
  const min = () => props.minWidth ?? 100;
  const max = () => props.maxWidth ?? 800;

  const [width, setWidth] = createSignal(props.initialWidth ?? 300);
  const [isResizing, setIsResizing] = createSignal(false);

  // `100vw` is a length, so the browser interpolates it from the dragged
  // pixel width when `.resizable-inner` transitions its width.
  const innerWidth = () => (props.expanded ? '100vw' : `${width()}px`);

  let rootEl: HTMLDivElement | undefined;

  // CSS `user-select: none` alone doesn't stop every drag-select (a drag
  // that starts in selectable text can run on into chrome, WebViews differ).
  // Cancelling `selectstart`/`dragstart` outside the opt-ins is the proven
  // fix — the same pattern FileTree already uses per-row via preventDefault
  // on mousedown.
  const onSelectStart = (e: Event) => {
    if (!isSelectable(e.target)) e.preventDefault();
  };
  const onDragStart = (e: Event) => {
    if (!isSelectable(e.target)) e.preventDefault();
  };

  function attachGuards(el: HTMLDivElement | undefined) {
    if (rootEl && rootEl !== el) {
      rootEl.removeEventListener('selectstart', onSelectStart);
      rootEl.removeEventListener('dragstart', onDragStart);
    }
    rootEl = el;
    // `selectstart` doesn't bubble through Solid's delegated onSelectStart
    // in every WebView, so listen directly.
    rootEl?.addEventListener('selectstart', onSelectStart);
    rootEl?.addEventListener('dragstart', onDragStart);
  }

  onCleanup(() => {
    rootEl?.removeEventListener('selectstart', onSelectStart);
    rootEl?.removeEventListener('dragstart', onDragStart);
  });

  const startResize = (e: MouseEvent) => {
    // Expanded, the divider is hidden and the panel's width is the window's:
    // a drag would have nothing to move.
    if (props.expanded) return;
    e.preventDefault();
    // A resize drag must never leave a text selection behind: drop any
    // existing one and suppress selection globally until mouseup.
    window.getSelection()?.removeAllRanges();
    document.body.classList.add('is-resizing');
    setIsResizing(true);
    window.addEventListener('mousemove', handleResize);
    window.addEventListener('mouseup', stopResize);
  };

  const handleResize = (e: MouseEvent) => {
    if (!isResizing()) return;
    const newWidth = Math.max(min(), Math.min(max(), e.clientX));
    setWidth(newWidth);
  };

  const stopResize = () => {
    setIsResizing(false);
    document.body.classList.remove('is-resizing');
    window.removeEventListener('mousemove', handleResize);
    window.removeEventListener('mouseup', stopResize);
  };

  onCleanup(() => {
    document.body.classList.remove('is-resizing');
    stopResize();
  });

  return (
    <div class="resizable-root" ref={attachGuards}>
      <div
        class={`${props.class ?? ''} resizable-inner`}
        classList={{ expanded: !!props.expanded, resizing: isResizing() }}
        style={{ width: innerWidth() }}
      >
        {props.children}

        {/* Floating full-window toggle: at the panel's right edge while
            collapsed, just outside the chat column's right border while
            expanded (placed by CSS, see .sidebar-expand-button.expanded) */}
        <Show when={props.showFloatingButton}>
          <button
            type="button"
            class="sidebar-expand-button"
            classList={{ expanded: !!props.expanded }}
            aria-pressed={!!props.expanded}
            title={props.expanded ? 'Collapse panel' : 'Expand panel'}
            aria-label={props.expanded ? 'Collapse panel' : 'Expand panel'}
            onClick={() => props.onExpandedChange?.(!props.expanded)}
          >
            <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
              <path
                d={props.expanded ? 'M10 3 5 8l5 5' : 'M6 3l5 5-5 5'}
                fill="none"
                stroke="currentColor"
                stroke-width="1.6"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </svg>
          </button>
        </Show>
      </div>

      {/* Resize Handle */}
      <div
        class="resizable-handle"
        classList={{ resizing: isResizing() }}
        onMouseDown={startResize}
      />
    </div>
  );
}
