// GitIcons.tsx
//
// The Git panel's icons (GitPanel.tsx, GitRepoSection.tsx): inline SVG,
// sized via currentColor so they inherit the button's or the text's color.

const iconProps = {
  width: 14,
  height: 14,
  viewBox: '0 0 24 24',
  fill: 'none',
  stroke: 'currentColor',
  'stroke-width': 2,
  'stroke-linecap': 'round' as const,
  'stroke-linejoin': 'round' as const,
};

/** Git hooks, in the ⋯ menu. */
export function IconHook() {
  return (
    <svg {...iconProps}>
      <polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2" />
    </svg>
  );
}

/** A branch. */
export function IconBranch() {
  return (
    <svg {...iconProps}>
      <line x1="6" y1="3" x2="6" y2="15" />
      <circle cx="18" cy="6" r="3" />
      <circle cx="6" cy="18" r="3" />
      <path d="M18 9a9 9 0 0 1-9 9" />
    </svg>
  );
}

/** The branch button's arrow: it opens the branch menu. */
export function IconChevronDown() {
  return (
    <svg {...iconProps} width={10} height={10}>
      <polyline points="6 9 12 15 18 9" />
    </svg>
  );
}

/** Fetch, and Retry. */
export function IconRefresh() {
  return (
    <svg {...iconProps}>
      <polyline points="23 4 23 10 17 10" />
      <polyline points="1 20 1 14 7 14" />
      <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15" />
    </svg>
  );
}

/** Pull. */
export function IconArrowDown() {
  return (
    <svg {...iconProps}>
      <line x1="12" y1="5" x2="12" y2="19" />
      <polyline points="19 12 12 19 5 12" />
    </svg>
  );
}

/** Push, or publish a branch. */
export function IconArrowUp() {
  return (
    <svg {...iconProps}>
      <line x1="12" y1="19" x2="12" y2="5" />
      <polyline points="5 12 12 5 19 12" />
    </svg>
  );
}

/** Commit. */
export function IconCheck() {
  return (
    <svg {...iconProps}>
      <polyline points="20 6 9 17 4 12" />
    </svg>
  );
}

/** An error, or an operation that needs finishing. */
export function IconAlert() {
  return (
    <svg {...iconProps}>
      <path d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z" />
      <line x1="12" y1="9" x2="12" y2="13" />
      <line x1="12" y1="17" x2="12.01" y2="17" />
    </svg>
  );
}

/** A folder that isn't a repository yet. */
export function IconFolderGit() {
  return (
    <svg {...iconProps} width={28} height={28}>
      <path d="M4 20h16a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.93a2 2 0 0 1-1.66-.9L9.34 3.9A2 2 0 0 0 7.68 3H4a2 2 0 0 0-2 2v13c0 1.1.9 2 2 2Z" />
      <circle cx="12" cy="13" r="1.6" />
    </svg>
  );
}

/** Something is running. */
export function IconSpinner(props: { size?: number }) {
  return (
    <svg {...iconProps} width={props.size ?? 14} height={props.size ?? 14} class="git-spin">
      <path d="M21 12a9 9 0 1 1-9-9" />
    </svg>
  );
}

/** More actions. */
export function IconDots() {
  return (
    <svg {...iconProps}>
      <circle cx="12" cy="5" r="1" />
      <circle cx="12" cy="12" r="1" />
      <circle cx="12" cy="19" r="1" />
    </svg>
  );
}

/** Back to the ⋯ menu's list. */
export function IconBack() {
  return (
    <svg {...iconProps} width={12} height={12}>
      <polyline points="15 18 9 12 15 6" />
    </svg>
  );
}

/** The stash. */
export function IconArchive() {
  return (
    <svg {...iconProps}>
      <rect x="2" y="3" width="20" height="5" rx="1" />
      <path d="M4 8v11a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1V8" />
      <path d="M10 12h4" />
    </svg>
  );
}

/** Recent commits. */
export function IconHistory() {
  return (
    <svg {...iconProps}>
      <path d="M3 12a9 9 0 1 0 3-6.7L3 8" />
      <path d="M3 3v5h5" />
      <path d="M12 7v5l3 2" />
    </svg>
  );
}

/** A commit moved up onto the tip of another line of commits. */
export function IconRebase() {
  return (
    <svg {...iconProps}>
      <circle cx="6" cy="18" r="2" />
      <circle cx="6" cy="6" r="2" />
      <line x1="6" y1="8" x2="6" y2="16" />
      <line x1="8" y1="6" x2="16" y2="6" />
      <circle cx="18" cy="6" r="2" />
      <path d="M18 19v-8" />
      <path d="m15.5 13.5 2.5-2.5 2.5 2.5" />
    </svg>
  );
}

/** Delete, forget or discard. */
export function IconTrash() {
  return (
    <svg {...iconProps}>
      <path d="M3 6h18" />
      <path d="M8 6V4a1 1 0 0 1 1-1h6a1 1 0 0 1 1 1v2" />
      <path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" />
    </svg>
  );
}
