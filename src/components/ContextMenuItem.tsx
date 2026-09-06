import { JSX } from 'solid-js';

export default function ContextMenuItem(props: {
  label: string | JSX.Element;
  onClick: () => void;
  isDanger?: boolean;
  disabled?: boolean;
}) {
  return (
    <li
      class="context-menu-item"
      classList={{ danger: props.isDanger, disabled: props.disabled }}
      onClick={(e) => {
        e.stopPropagation(); // Avoid triggering global handlers
        if (props.disabled) return;
        props.onClick();
      }}
    >
      {props.label}
    </li>
  );
}
