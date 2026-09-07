import { createSignal, onCleanup, JSX } from 'solid-js';

interface ResizableProps {
  children: JSX.Element;
  class?: string;
  initialWidth?: number;
  minWidth?: number;
  maxWidth?: number;
}

export default function Resizable(props: ResizableProps) {
  const min = () => props.minWidth ?? 100;
  const max = () => props.maxWidth ?? 800;

  const [width, setWidth] = createSignal(props.initialWidth ?? 300);
  const [isResizing, setIsResizing] = createSignal(false);

  const startResize = (e: MouseEvent) => {
    e.preventDefault();
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
    window.removeEventListener('mousemove', handleResize);
    window.removeEventListener('mouseup', stopResize);
  };

  onCleanup(() => stopResize());

  return (
    <div class="resizable-root">
      <div class={`${props.class ?? ''} resizable-inner`} style={{ width: `${width()}px` }}>
        {props.children}
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
