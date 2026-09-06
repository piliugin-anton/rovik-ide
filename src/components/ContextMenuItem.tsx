export default function ContextMenuItem(props: {
  label: string;
  onClick: () => void;
  isDanger?: boolean;
}) {
  return (
    <li
      class="context-menu-item"
      classList={{ danger: props.isDanger }}
      onClick={(e) => {
        e.stopPropagation(); // Avoid triggering global handlers
        props.onClick();
      }}
    >
      {props.label}
    </li>
  );
}
