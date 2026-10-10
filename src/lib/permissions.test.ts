import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { checkCommand, DEFAULT_PERMISSIONS, parseCommand } from './permissions';

/*
 * The cases are shared with the backend's tests (agent_run/permissions.rs):
 * the backend enforces these rules, this file only hints at them in the
 * UI, and both must agree on every case.
 */
interface Case {
  command: string;
  allowed?: string[];
  allowAny?: boolean;
  expect: {
    segments?: string[][];
    writesViaRedirect?: boolean;
    opaque?: boolean;
    permitted?: boolean;
    modifiesFiles?: boolean;
  };
}
const fixture = JSON.parse(
  readFileSync(join(process.cwd(), 'test-fixtures/command-permissions.json'), 'utf8')
) as { defaultAllowed: string[]; cases: Case[] };

test('commands parse and are judged as the backend judges them', () => {
  for (const c of fixture.cases) {
    const at = JSON.stringify(c.command);
    const parsed = parseCommand(c.command);
    if (c.expect.segments)
      assert.deepEqual(parsed.segments, c.expect.segments, `segments of ${at}`);
    if (c.expect.writesViaRedirect !== undefined)
      assert.equal(parsed.writesViaRedirect, c.expect.writesViaRedirect, `redirect in ${at}`);
    if (c.expect.opaque !== undefined) assert.equal(parsed.opaque, c.expect.opaque, `opaque ${at}`);

    const verdict = checkCommand(c.command, {
      ...DEFAULT_PERMISSIONS,
      allowAnyCommand: c.allowAny ?? false,
      allowedCommands: c.allowed ?? fixture.defaultAllowed,
    });
    if (c.expect.permitted !== undefined)
      assert.equal(verdict.permitted, c.expect.permitted, `permitted ${at}`);
    if (c.expect.modifiesFiles !== undefined) {
      assert.ok(verdict.permitted, `${at} must be permitted to say whether it modifies files`);
      if (verdict.permitted)
        assert.equal(verdict.modifiesFiles, c.expect.modifiesFiles, `modifies files ${at}`);
    }
    if (!verdict.permitted) assert.ok(verdict.reason.length > 0, `a reason for ${at}`);
  }
});
