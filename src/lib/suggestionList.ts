// suggestionList.ts
//
// The composer's lists under its input: the `@` files and the `/` commands.
// The box is a scroll area much shorter than the list (styles.css:
// .file-suggestions, max-height 280px), and the keys move the pick, so the
// list has to follow it: the picked row would otherwise walk out of sight
// and the arrows would look dead past the rows on screen. The rows have no
// fixed height (a name, a path, a "Selection" tag), so this measures
// nothing: the browser scrolls the box just enough to show the row.

/**
 * Scrolls `list` so its `index`-th row is in view, and leaves an already
 * visible row where it is. Does nothing when there is no such row: the list
 * is closed, empty (index -1), or the row is no longer in it.
 */
export function revealSuggestionRow(list: HTMLElement | null | undefined, index: number): void {
  if (!list || index < 0) return;
  // By the rows' class, not by position among the children: the `/` list
  // puts a status line above its rows.
  const row = list.querySelectorAll('.file-suggestion')[index];
  row?.scrollIntoView({ block: 'nearest' });
}
