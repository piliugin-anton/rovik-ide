import { createSignal } from 'solid-js';

export default function ContextMenuItem(props: {
  label: string;
  onClick: () => void;
  isDanger?: boolean;
}) {
  const [hovered, setHovered] = createSignal(false);

  return (
    <li
      onClick={(e) => {
        e.stopPropagation(); // Avoid triggering global handlers
        props.onClick();
      }}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
      style={{
        padding: '8px 14px',
        cursor: 'pointer',
        transition: 'background 0.15s ease',
        background: hovered() ? (props.isDanger ? '#e03e3e' : '#3b3b4a') : 'transparent',
        color: props.isDanger && !hovered() ? '#ff6b6b' : '#fff',
        'font-size': '14px',
      }}
    >
      {props.label}
    </li>
  );
}
