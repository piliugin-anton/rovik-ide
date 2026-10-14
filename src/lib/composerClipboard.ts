// composerClipboard.ts
//
// Copying from the chat input or a sent message keeps its chips: pasted
// back into the input, its images, PDFs, attached files, file references
// and long pastes come back as chips, not as their text (which, long, would
// come back as one "Pasted text" chip).
//
// A copy puts the segments (composerSegments.ts) on the clipboard in a type
// of the app's own, beside the plain text other apps get. Not every way of
// pasting can read that type (the async clipboard API, which the input's
// menu uses where the webview won't paste by itself), so the last copy is
// also kept here and known again by its plain text, as editors do.

import { asSegment, plainText, type DraftSegment } from './composerSegments';

const SEGMENTS_TYPE = 'application/x-rovik-segments+json';

let lastCopy: { text: string; segments: DraftSegment[] } | null = null;

const sameText = (a: string, b: string) => a.replace(/\r\n?/g, '\n') === b.replace(/\r\n?/g, '\n');

/** Only a copy with a chip in it needs more than its text. */
const hasChips = (segments: DraftSegment[]) => segments.some((s) => s.type !== 'text');

function remember(text: string, segments: DraftSegment[]) {
  lastCopy = hasChips(segments) ? { text, segments } : null;
}

/** Puts segments on a copy or cut event's clipboard. */
export function writeSegments(data: DataTransfer, segments: DraftSegment[]): void {
  const text = plainText(segments);
  data.setData('text/plain', text);
  if (hasChips(segments)) data.setData(SEGMENTS_TYPE, JSON.stringify(segments));
  remember(text, segments);
}

/**
 * Copies segments outside a copy event (a message's Copy button): the
 * webview's own copy, written as a copy in the input is, else the plain text
 * through the clipboard API. False when the clipboard couldn't be written.
 */
export async function copySegments(segments: DraftSegment[]): Promise<boolean> {
  let written = false;
  const onCopy = (e: ClipboardEvent) => {
    if (!e.clipboardData) return;
    e.preventDefault();
    // Not for the input's own handler, should it have the focus.
    e.stopImmediatePropagation();
    writeSegments(e.clipboardData, segments);
    written = true;
  };
  document.addEventListener('copy', onCopy, true);
  try {
    document.execCommand('copy');
  } catch {
    // Not here: the clipboard API below.
  } finally {
    document.removeEventListener('copy', onCopy, true);
  }
  if (written) return true;
  const text = plainText(segments);
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    return false;
  }
  remember(text, segments);
  return true;
}

/** Segments as a copy wrote them; null when that isn't what the text is. */
function parseSegments(raw: string): DraftSegment[] | null {
  try {
    const value: unknown = JSON.parse(raw);
    if (!Array.isArray(value)) return null;
    const segments = value.map(asSegment);
    return segments.every((s): s is DraftSegment => s !== null) ? segments : null;
  } catch {
    return null;
  }
}

/** The last copy's segments, when the clipboard's text is still that copy's. */
export function copiedSegments(text: string): DraftSegment[] | null {
  return lastCopy && text && sameText(text, lastCopy.text) ? lastCopy.segments : null;
}

/** What a paste carries when it's a copy of the chat input or a message; null otherwise. */
export function pastedSegments(data: DataTransfer | null): DraftSegment[] | null {
  if (!data) return null;
  const raw = data.getData(SEGMENTS_TYPE);
  return (raw ? parseSegments(raw) : null) ?? copiedSegments(data.getData('text/plain'));
}
