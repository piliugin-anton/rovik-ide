import { createSignal, onCleanup, JSX } from 'solid-js';

interface ResizableProps {
  children: JSX.Element;
  class?: string; // Accepts the custom class string
  initialWidth?: number;
  minWidth?: number;
  maxWidth?: number;
}

export default function Resizable(props: ResizableProps) {
  const min = () => props.minWidth ?? 100;
  const max = () => props.maxWidth ?? 800;

  const [width, setWidth] = createSignal(props.initialWidth ?? 300);
  let isResizing = false;

  const startResize = (e: MouseEvent) => {
    e.preventDefault();
    isResizing = true;
    window.addEventListener('mousemove', handleResize);
    window.addEventListener('mouseup', stopResize);
  };

  const handleResize = (e: MouseEvent) => {
    if (!isResizing) return;
    const newWidth = Math.max(min(), Math.min(max(), e.clientX));
    setWidth(newWidth);
  };

  const stopResize = () => {
    isResizing = false;
    window.removeEventListener('mousemove', handleResize);
    window.removeEventListener('mouseup', stopResize);
  };

  onCleanup(() => stopResize());

  return (
    <div style={{ display: 'flex', height: '100%', 'min-height': '200px' }}>
      {/* 
        The parent component passes props.class here. 
        Inner container is a flex column that fills the height,
        allowing children (like ChatPanel) to use flex layout with scrolling.
      */}
      <div
        class={`${props.class ?? ''}`}
        style={{
          width: `${width()}px`,
          'box-sizing': 'border-box',
          display: 'flex',
          'flex-direction': 'column',
          height: '100%',
          overflow: 'hidden',
        }}
      >
        {props.children}
      </div>

      {/* Resize Handle */}
      <div
        onMouseDown={startResize}
        style={{
          width: '2px',
          cursor: 'col-resize',
          background: '#cbd5e1',
          transition: 'background 0.2s',
          'margin-left': '2px',
          'margin-right': '2px',
        }}
        onMouseOver={(e) => (e.currentTarget.style.background = '#3b82f6')}
        onMouseOut={(e) => (e.currentTarget.style.background = '#cbd5e1')}
      />
    </div>
  );
}
