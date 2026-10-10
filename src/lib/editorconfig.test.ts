import { test } from 'node:test';
import assert from 'node:assert/strict';
import { globMatches, resolveEditorConfig } from './editorconfig';

test('globs without a slash match the file name at any depth', () => {
  assert.ok(globMatches('*', 'a.ts'));
  assert.ok(globMatches('*.ts', 'src/lib/a.ts'));
  assert.ok(!globMatches('*.ts', 'src/lib/a.tsx'));
  assert.ok(globMatches('Makefile', 'sub/Makefile'));
  assert.ok(globMatches('?.go', 'x/a.go'));
  assert.ok(!globMatches('?.go', 'x/ab.go'));
});

test('globs with a slash are anchored to the .editorconfig folder', () => {
  assert.ok(globMatches('src/*.ts', 'src/a.ts'));
  assert.ok(!globMatches('src/*.ts', 'pkg/src/a.ts'));
  assert.ok(!globMatches('src/*.ts', 'src/lib/a.ts'));
  assert.ok(globMatches('/src/**.ts', 'src/lib/a.ts'));
  assert.ok(globMatches('lib/**/a.ts', 'lib/a.ts'));
  assert.ok(globMatches('lib/**/a.ts', 'lib/x/y/a.ts'));
});

test('braces, brackets and numeric ranges', () => {
  assert.ok(globMatches('*.{js,ts}', 'a.ts'));
  assert.ok(globMatches('*.{js,ts}', 'a.js'));
  assert.ok(!globMatches('*.{js,ts}', 'a.rs'));
  assert.ok(globMatches('{package.json,.travis.yml}', 'package.json'));
  assert.ok(globMatches('*.[ch]', 'a.h'));
  assert.ok(!globMatches('*.[!ch]', 'a.c'));
  assert.ok(globMatches('file{1..3}.txt', 'file2.txt'));
  assert.ok(!globMatches('file{1..3}.txt', 'file4.txt'));
  assert.ok(globMatches('{single}.txt', '{single}.txt'));
});

const file = (dir: string, content: string) => ({ dir, content });

test('nearer files and later sections win; root stops the search', () => {
  const files = [
    file('/p/src', '[*.ts]\nindent_size = 4\n'),
    file(
      '/p',
      'root = true\n[*]\nindent_style = space\nindent_size = 2\n[*.md]\ntrim_trailing_whitespace = false\n'
    ),
    file('/', '[*]\nend_of_line = crlf\n'),
  ];
  assert.deepEqual(resolveEditorConfig(files, '/p/src/a.ts'), {
    indentStyle: 'space',
    indentSize: 4,
    tabWidth: 4,
  });
  assert.deepEqual(resolveEditorConfig(files, '/p/README.md'), {
    indentStyle: 'space',
    indentSize: 2,
    tabWidth: 2,
    trimTrailingWhitespace: false,
  });
});

test('tab indentation and unset', () => {
  const files = [
    file(
      '/p',
      '[*]\nindent_style = tab\ntab_width = 8\ninsert_final_newline = true\n[*.go]\ninsert_final_newline = unset\n'
    ),
  ];
  assert.deepEqual(resolveEditorConfig(files, '/p/main.c'), {
    indentStyle: 'tab',
    indentSize: 8,
    tabWidth: 8,
    insertFinalNewline: true,
  });
  assert.deepEqual(resolveEditorConfig(files, '/p/main.go'), {
    indentStyle: 'tab',
    indentSize: 8,
    tabWidth: 8,
  });
});

test('Windows paths and comments', () => {
  const files = [
    file('C:\\p', '# comment\n; another\n[*.py]\nindent_size = 4\nEnd_Of_Line = LF\n'),
  ];
  assert.deepEqual(resolveEditorConfig(files, 'C:\\p\\pkg\\a.py'), {
    indentSize: 4,
    tabWidth: 4,
    endOfLine: 'lf',
  });
  assert.deepEqual(resolveEditorConfig(files, 'C:\\p\\a.rs'), {});
});
