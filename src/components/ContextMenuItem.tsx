import { Show, type JSX } from 'solid-js';

export default function ContextMenuItem(props: {
  icon?: JSX.Element;
  label: string;
  /** Shown on the right, e.g. "Ctrl+C". */
  shortcut?: string;
  onClick: () => void;
  isDanger?: boolean;
  disabled?: boolean;
}) {
  return (
    <li
      class="context-menu-item"
      classList={{ danger: props.isDanger, disabled: props.disabled }}
      role="menuitem"
      aria-disabled={props.disabled}
      onClick={(e) => {
        e.stopPropagation(); // Avoid triggering global handlers
        if (props.disabled) return;
        props.onClick();
      }}
    >
      <span class="context-menu-icon" aria-hidden="true">
        {props.icon}
      </span>
      <span class="context-menu-label">{props.label}</span>
      <Show when={props.shortcut}>
        <span class="context-menu-shortcut">{props.shortcut}</span>
      </Show>
    </li>
  );
}
