import { createSignal, Show, type Accessor } from 'solid-js';
import Terminal from './Terminal';

interface TerminalPanelProps {
  collapsed: Accessor<boolean>;
  onCollapsedChange: (value: boolean) => void;
  rootPath: Accessor<string | null>;
}

const DEFAULT_HEIGHT = 240;
const MIN_HEIGHT = 120;
const MAX_HEIGHT = 640;

export default function TerminalPanel(props: TerminalPanelProps) {
  const [mounted, setMounted] = createSignal(!props.collapsed());
  const [height, setHeight] = createSignal(DEFAULT_HEIGHT);
  const [dragging, setDragging] = createSignal(false);

  function toggle() {
    const next = !props.collapsed();

    // Mount before/while expanding.
    if (!mounted()) {
      setMounted(true);
    }

    props.onCollapsedChange(next);
  }

  function startResize(e: PointerEvent) {
    if (props.collapsed()) return;
    e.preventDefault();
    setDragging(true);
    const startY = e.clientY;
    const startHeight = height();

    function onMove(ev: PointerEvent) {
      const delta = startY - ev.clientY;
      setHeight(Math.min(MAX_HEIGHT, Math.max(MIN_HEIGHT, startHeight + delta)));
    }
    function onUp() {
      setDragging(false);
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
    }
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  }

  return (
    <div class="terminal-panel" classList={{ collapsed: props.collapsed() }}>
      <Show when={!props.collapsed()}>
        <div
          class="terminal-resize-handle"
          classList={{ dragging: dragging() }}
          onPointerDown={startResize}
        />
      </Show>

      <div class="terminal-panel-header">
        <button
          class="terminal-toggle-button"
          onClick={toggle}
          title={props.collapsed() ? 'Expand terminal' : 'Collapse terminal'}
        >
          <span class="terminal-toggle-icon" classList={{ expanded: !props.collapsed() }}>
            &#9656;
          </span>
          Terminal
        </button>
      </div>

      <div
        class="terminal-panel-body"
        classList={{ 'no-transition': dragging() }}
        style={{ height: props.collapsed() ? '0px' : `${height()}px` }}
      >
        <Show when={mounted()}>
          <Terminal cwd={props.rootPath()} />
        </Show>
      </div>
    </div>
  );
}
