// bashLine.ts tests (node:test): which keys a terminal sends bash's start
// file around what's typed and pasted, from what bash says of its line.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  CHECK,
  NO_LINE,
  ONE_ROW,
  keyKind,
  lineAfter,
  lineDone,
  mayOverflowResized,
  parseBashSaid,
  pasteMayOverflow,
  withBashKeys,
  type BashLine,
} from './bashLine';

const paste = (text: string) => `\x1b[200~${text}\x1b[201~`;
const atPrompt = (vi = false): BashLine => lineAfter(NO_LINE, { kind: 'prompt', vi });
const oneRow = (need: number): BashLine => lineAfter(atPrompt(), { kind: 'one-row', need });
const tooTall = () => true;
const fits = () => false;

test('what bash says of its line: its own words alone', () => {
  assert.deepEqual(parseBashSaid('rovik;prompt;emacs'), { kind: 'prompt', vi: false });
  assert.deepEqual(parseBashSaid('rovik;prompt;vi'), { kind: 'prompt', vi: true });
  assert.deepEqual(parseBashSaid('rovik;one-row;250'), { kind: 'one-row', need: 250 });
  assert.deepEqual(parseBashSaid('rovik;one-row;0'), { kind: 'one-row', need: 0 });
  assert.deepEqual(parseBashSaid('rovik;wrap'), { kind: 'wrap' });
  assert.equal(parseBashSaid('rovik;later'), null);
  assert.equal(parseBashSaid('someone;prompt'), null);
  assert.equal(parseBashSaid(''), null);
});

test('a prompt starts afresh; the line editor done with the line, nothing goes', () => {
  const before: BashLine = { prompt: true, away: true, oneRow: true, need: 4, vi: false };
  assert.deepEqual(lineAfter(before, { kind: 'prompt', vi: true }), {
    prompt: true,
    away: false,
    oneRow: false,
    need: 0,
    vi: true,
  });
  assert.equal(lineDone(oneRow(3)).prompt, false);
  // Outside bash's prompt (a program running, a shell without the file): as typed.
  for (const line of [NO_LINE, lineDone(oneRow(3))]) {
    for (const data of ['\x7f', paste('x'.repeat(5000)), 'a', '\r']) {
      assert.deepEqual(withBashKeys(line, data, tooTall), { data, line, keys: 0 });
    }
  }
});

test('keys, by what they do to the line', () => {
  for (const key of ['\x7f', '\x08', '\x1b[3~']) assert.equal(keyKind(key, false), 'delete');
  for (const key of ['\x17', '\x15', '\x0b', '\x1b\x7f', '\x1b\x08', '\x1bd']) {
    assert.equal(keyKind(key, false), 'kill', JSON.stringify(key));
  }
  for (const key of ['\x1b[D', '\x1bOC', '\x1b[H', '\x1b[4~', '\x01', '\x05', '\x0c', '\x1bb']) {
    assert.equal(keyKind(key, false), 'move', JSON.stringify(key));
  }
  for (const text of ['a', 'hello', 'привет', '日本', ' '])
    assert.equal(keyKind(text, false), 'text');
  assert.equal(keyKind(paste('two\rlines'), false), 'paste');
  // Enter, Tab, Escape, Ctrl+R, Ctrl+C, up, a function key, a mouse report.
  for (const key of ['\r', '\t', '\x1b', '\x12', '\x03', '\x1b[A', '\x1bOP', '\x1b[M !!']) {
    assert.equal(keyKind(key, false), 'other', JSON.stringify(key));
  }
  // In vi mode Escape leaves insert mode: Alt+key is no kill nor move there.
  for (const key of ['\x1b\x7f', '\x1bd', '\x1bb']) {
    assert.equal(keyKind(key, true), 'other', JSON.stringify(key));
  }
  assert.equal(keyKind('\x17', true), 'kill');
});

test('a paste that may not fit goes on one row first, and is checked after', () => {
  const line = atPrompt();
  const long = paste('x'.repeat(2000));
  assert.deepEqual(withBashKeys(line, long, tooTall), {
    data: ONE_ROW + long + CHECK,
    line,
    keys: 2,
  });
  // One that fits goes as it is.
  assert.deepEqual(withBashKeys(line, paste('ls'), fits), { data: paste('ls'), line, keys: 0 });
  // On one row already: checked after, for how much must go now.
  assert.deepEqual(withBashKeys(oneRow(3), long, fits), {
    data: long + CHECK,
    line: oneRow(3),
    keys: 1,
  });
  // The paste's own text is what's measured.
  let measured = '';
  withBashKeys(line, paste('a\rb'), (text) => ((measured = text), false));
  assert.equal(measured, 'a\rb');
});

test('deleting on one row: checked once the line may fit', () => {
  // Two characters to go, a deletion each.
  let line = oneRow(2);
  let sent = withBashKeys(line, '\x7f', fits);
  assert.deepEqual(sent, { data: '\x7f', line: { ...line, need: 1 }, keys: 0 });
  sent = withBashKeys(sent.line, '\x7f', fits);
  assert.deepEqual(sent, { data: '\x7f' + CHECK, line: { ...line, need: 0 }, keys: 1 });
  // Until bash says, each deletion is checked.
  sent = withBashKeys(sent.line, '\x08', fits);
  assert.equal(sent.data, '\x08' + CHECK);
  // A kill may take many rows: always checked.
  line = oneRow(40);
  for (const kill of ['\x17', '\x15', '\x0b', '\x1b\x7f', '\x1bd']) {
    assert.equal(withBashKeys(line, kill, fits).data, kill + CHECK, JSON.stringify(kill));
  }
  // Each character typed is one more to delete.
  sent = withBashKeys(oneRow(1), 'abc', fits);
  assert.deepEqual(sent, { data: 'abc', line: { ...oneRow(1), need: 4 }, keys: 0 });
  // Wrapping, deleting needs nothing.
  assert.deepEqual(withBashKeys(atPrompt(), '\x7f', fits), {
    data: '\x7f',
    line: atPrompt(),
    keys: 0,
  });
  // Bash says it fits: wrapping again, nothing more.
  line = lineAfter(oneRow(1), { kind: 'wrap' });
  assert.deepEqual(withBashKeys(line, '\x7f', fits), { data: '\x7f', line, keys: 0 });
});

test('after a key that may take the editor elsewhere, nothing more until the next prompt', () => {
  // Ctrl+R: a search, where Escape would end it and the rest go into it.
  let sent = withBashKeys(oneRow(1), '\x12', fits);
  assert.deepEqual(sent, { data: '\x12', line: { ...oneRow(1), away: true }, keys: 0 });
  for (const data of ['\x7f', '\x15', paste('x'.repeat(5000)), 'a']) {
    assert.equal(withBashKeys(sent.line, data, tooTall).data, data);
  }
  // Moves keep the editor where it is.
  sent = withBashKeys(oneRow(1), '\x1b[D', fits);
  assert.equal(sent.line.away, false);
  assert.equal(withBashKeys(sent.line, '\x7f', fits).data, '\x7f' + CHECK);
  // Enter: the line goes; Tab: a completion may ask.
  for (const key of ['\r', '\t']) {
    assert.equal(withBashKeys(atPrompt(), key, tooTall).line.away, true);
  }
  // The next prompt: back.
  const back = lineAfter(sent.line, { kind: 'prompt', vi: false });
  assert.equal(back.away, false);
});

test('a paste that may not fit: by its rows, and the line it goes in', () => {
  // A 100×12 terminal: 11 rows the line can take. A line of 1 row, caret at 10.
  assert.equal(pasteMayOverflow('x'.repeat(800), 100, 12, 1, 10), false);
  assert.equal(pasteMayOverflow('x'.repeat(1000), 100, 12, 1, 10), true);
  // Its line breaks are rows.
  assert.equal(pasteMayOverflow('a\r'.repeat(8), 100, 12, 1, 10), false);
  assert.equal(pasteMayOverflow('a\r'.repeat(10), 100, 12, 1, 10), true);
  // A line taller already has fewer to spare.
  assert.equal(pasteMayOverflow('x'.repeat(500), 100, 12, 6, 10), true);
  // Wide characters take two columns.
  assert.equal(pasteMayOverflow('日'.repeat(400), 100, 12, 1, 0), false);
  assert.equal(pasteMayOverflow('日'.repeat(500), 100, 12, 1, 0), true);
  assert.equal(pasteMayOverflow('ж'.repeat(800), 100, 12, 1, 0), false);
  // A huge one is looked at no further than it takes.
  assert.equal(pasteMayOverflow('x'.repeat(5_000_000), 100, 12, 1, 0), true);
});

test('a resize that may leave the line too tall', () => {
  // A line of 3 rows of 100: taller than fits in 3 rows, not in 12.
  assert.equal(mayOverflowResized(3, 100, 100, 3), true);
  assert.equal(mayOverflowResized(3, 100, 100, 12), false);
  // Half as wide: twice the rows, and one.
  assert.equal(mayOverflowResized(3, 100, 50, 8), false);
  assert.equal(mayOverflowResized(3, 100, 50, 7), true);
  // Wider: no more rows.
  assert.equal(mayOverflowResized(5, 80, 120, 7), false);
});
