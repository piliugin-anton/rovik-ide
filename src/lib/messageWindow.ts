// messageWindow.ts
//
// Which slice of a chat's loaded messages is rendered. Pure functions, used
// by MessageList and tested on their own.
//
// The window holds at most MAX messages. Scrolling near the top or bottom
// moves it by STEP; new messages extend it at the end while the user follows
// the conversation, trimming the oldest so the DOM never grows unbounded.

export const WINDOW_MAX = 150;
export const WINDOW_INITIAL = 60;
export const WINDOW_STEP = 50;

export interface MessageWindow {
  start: number;
  end: number; // exclusive
}

export function initialWindow(length: number, size = WINDOW_INITIAL): MessageWindow {
  return { start: Math.max(0, length - size), end: length };
}

export function clampWindow(w: MessageWindow, length: number): MessageWindow {
  const end = Math.min(Math.max(w.end, 0), length);
  const start = Math.min(Math.max(w.start, 0), end);
  return { start, end };
}

/** Show earlier messages (already loaded); trims the far end past MAX. */
export function shiftUp(w: MessageWindow, step = WINDOW_STEP, max = WINDOW_MAX): MessageWindow {
  const start = Math.max(0, w.start - step);
  return { start, end: Math.min(w.end, start + max) };
}

/** Show later messages; trims the start past MAX. */
export function shiftDown(
  w: MessageWindow,
  length: number,
  step = WINDOW_STEP,
  max = WINDOW_MAX
): MessageWindow {
  const end = Math.min(length, w.end + step);
  return { start: Math.max(w.start, end - max), end };
}

/** Older messages were prepended: the same messages moved by `added`. */
export function afterPrepend(w: MessageWindow, added: number): MessageWindow {
  return { start: w.start + added, end: w.end + added };
}

/**
 * Messages were appended. While the window reaches the end and the user
 * follows the conversation, it extends to include them.
 */
export function afterAppend(
  w: MessageWindow,
  previousLength: number,
  length: number,
  following: boolean,
  max = WINDOW_MAX
): MessageWindow {
  if (!following || w.end < previousLength) return w;
  return { start: Math.max(w.start, length - max), end: length };
}
