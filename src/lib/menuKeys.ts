// menuKeys.ts
//
// The keys of the app's small menus (TerminalNewMenu, OpenedPorts): the
// arrows, Home and End move the focus around the menu's items, Enter or
// Space picks the item in focus, and Escape closes the menu, as Tab does
// on its way to what comes after.

/** The menu's items: `role="menuitem"`, or one of its kinds. */
function itemsOf(menu: HTMLElement | undefined): HTMLElement[] {
  return [...(menu?.querySelectorAll<HTMLElement>('[role^="menuitem"]') ?? [])];
}

/** The focus on the item of `menu` at `index`, counted around from either end. */
export function focusMenuItem(menu: HTMLElement | undefined, index: number): void {
  const items = itemsOf(menu);
  items[(index + items.length) % items.length]?.focus({ preventScroll: true });
}

/** `e`, a key pressed in `menu`, done: the focus moved, an item picked, or `close`d. */
export function onMenuKey(e: KeyboardEvent, menu: HTMLElement | undefined, close: () => void) {
  const items = itemsOf(menu);
  const at = items.indexOf(document.activeElement as HTMLElement);
  switch (e.key) {
    case 'ArrowDown':
      focusMenuItem(menu, at + 1);
      break;
    case 'ArrowUp':
      focusMenuItem(menu, at < 0 ? -1 : at - 1);
      break;
    case 'Home':
      focusMenuItem(menu, 0);
      break;
    case 'End':
      focusMenuItem(menu, -1);
      break;
    case 'Enter':
    case ' ':
      items[at]?.click();
      break;
    case 'Escape':
      close();
      break;
    case 'Tab':
      // The focus goes on to what comes after the menu's button.
      close();
      return;
    default:
      return;
  }
  e.preventDefault();
  e.stopPropagation();
}
