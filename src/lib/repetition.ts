// repetition.ts
//
// Notices a reply that got stuck repeating itself while it streams ("Sigh.
// I'll just output it." forty times, or the same plan over and over), so
// the request can be stopped instead of running to its output limit. Two
// shapes are caught:
//   - a short unit (a phrase, a line) repeated many times in a row at the end;
//   - a long paragraph that keeps coming back (outside code blocks, where
//     repeated lines are normal).
// Returns where the repetition starts, to cut the reply there.

/** A unit of 4 to 400 characters repeated at least this many times in a row. */
const UNIT_REPEATS = 6;
/** A paragraph of at least 80 characters seen at least this many times. */
const PARAGRAPH_REPEATS = 3;
const WINDOW = 8000;

export function findRepetition(text: string): number | null {
  if (text.length < 120) return null;
  const raw = text.slice(-WINDOW);
  const offset = text.length - raw.length;
  // Code is blanked out (same length, so offsets still match): repeated
  // lines and values are normal there (`0x00, 0x00, …`).
  const tail = raw.replace(/```[\s\S]*?(```|$)/g, (m) => ' '.repeat(m.length));

  // 1. The end is one unit repeated (whitespace between them included in the unit).
  for (let p = 4; p <= 400 && p * UNIT_REPEATS <= tail.length; p++) {
    const unit = tail.slice(-p);
    // Words, not punctuation: a wide table's "| --- | --- |" row isn't a loop.
    if (unit.replace(/[^\p{L}\p{N}]/gu, '').length < 6) continue;
    let k = 1;
    while (
      (k + 1) * p <= tail.length &&
      tail.slice(tail.length - (k + 1) * p, tail.length - k * p) === unit
    )
      k++;
    if (k >= UNIT_REPEATS) {
      // Keep the first occurrence.
      return offset + tail.length - k * p + p;
    }
  }

  // 2. Long paragraphs that keep coming back, outside code.
  const prose = tail;
  const seen = new Map<string, number[]>();
  const re = /[^\n]+(?:\n(?!\s*\n)[^\n]+)*/g;
  for (let m = re.exec(prose); m; m = re.exec(prose)) {
    const para = m[0].trim();
    if (para.length < 80) continue;
    const at = seen.get(para) ?? [];
    at.push(m.index);
    seen.set(para, at);
    if (at.length >= PARAGRAPH_REPEATS) return offset + at[1];
  }
  return null;
}
