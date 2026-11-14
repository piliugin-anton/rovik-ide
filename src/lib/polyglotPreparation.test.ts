import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  PROFILE_FACTS,
  suggestedProfile,
  suggestedWorkspaceSetup,
  workspaceSetupProblem,
} from './polyglotPreparation';
function setup() {
  const value = suggestedWorkspaceSetup('model');
  Object.assign(value, {
    name: 'Quota',
    intent: 'Preserve quota',
    boundedContext: 'Reservations',
    writePaths: ['src/**'],
    protectedPaths: ['tests/**'],
  });
  value.profiles = [
    suggestedProfile('typescript', 'ui', 'package.json'),
    suggestedProfile('rust', 'core', 'src-tauri/Cargo.toml'),
  ];
  for (const profile of value.profiles) {
    for (const key of PROFILE_FACTS[profile.language])
      profile.facts[key] = 'explicit-reviewed-value';
    profile.protectedTestPaths = [`tests/${profile.id}.test`];
  }
  value.requirements = [
    {
      id: 'quota',
      statement: 'Quota is preserved',
      owner: 'quota',
      casesByConfiguration: { 'ui/ui-tests': ['ui-quota'], 'core/core-tests': ['core-quota'] },
    },
  ];
  value.environment = {
    imageDigest: 'sha256:pinned',
    engine: 'podman',
    toolchainManifestPath: '/host/toolchain.txt',
    dependencyDirectory: '/host/dependencies',
  };
  value.negativeControlRevision = 'known-bug';
  value.negativeControlCases = ['core-quota'];
  value.design = {
    components: { quota: 'Owns reservations' },
    existingCode: [{ file: 'src/quota.ts', symbol: 'reserve', rationale: 'Reuse existing owner' }],
    publicApiChanges: [],
    resourceContracts: [],
    errorContracts: [],
    concurrencyContract: 'Serial',
    mechanismPressure: 'Direct function',
    mechanismAlternatives: ['Existing function'],
    mechanismCost: 'No persistent state',
  };
  return value;
}
test('a mixed Rust and TypeScript setup requires concrete cases for every configuration', () => {
  const value = setup();
  assert.equal(workspaceSetupProblem(value), null);
  value.profiles[0].configurations.push({
    ...value.profiles[0].configurations[0],
    id: 'alternate',
  });
  assert.match(workspaceSetupProblem(value)!, /every profile configuration/);
  value.requirements[0].casesByConfiguration['ui/alternate'] = ['ui-alternate'];
  assert.equal(workspaceSetupProblem(value), null);
});
test('manifest suggestions never resolve missing language facts or protected test inputs', () => {
  const value = setup();
  delete value.profiles[1].facts.panic_policy;
  assert.match(workspaceSetupProblem(value)!, /every rust profile fact/);
  const runtime = suggestedProfile('python');
  assert.equal(runtime.facts.test_runner, 'pytest');
  assert.equal(runtime.facts.python_version, undefined);
  assert.deepEqual(runtime.protectedTestPaths, []);
});
