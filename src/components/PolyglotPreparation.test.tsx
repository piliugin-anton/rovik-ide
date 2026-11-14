import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import PolyglotPreparation from './PolyglotPreparation';
import type { WorkspaceSetupInput } from '../lib/polyglotPreparation';

function fill(root: HTMLElement, label: string, value: string, list = false) {
  const element = root.querySelector<HTMLInputElement | HTMLTextAreaElement>(
    `[aria-label="${label}"]`
  );
  assert.ok(element, label);
  element.value = value;
  element.dispatchEvent(new Event(list ? 'change' : 'input', { bubbles: true }));
}
function button(root: HTMLElement, label: string) {
  const element = [...root.querySelectorAll('button')].find(
    (entry) => entry.textContent?.trim() === label
  );
  assert.ok(element, label);
  return element;
}

test('guided setup submits typed profile pins and case mapping without hand-authored JSON', () => {
  const root = container();
  let submitted: WorkspaceSetupInput | undefined;
  const dispose = render(
    () => (
      <PolyglotPreparation
        model="worker"
        manifests={[{ manifest: 'package.json', languages: ['javascript'] }]}
        busy={false}
        onPrepare={async (input) => {
          submitted = input;
        }}
      />
    ),
    root
  );
  try {
    assert.equal(button(root, 'Prepare this setup').disabled, true);
    fill(root, 'Setup name', 'Quota');
    fill(root, 'Acceptance intent', 'Preserve quota');
    fill(root, 'Bounded context', 'Reservations');
    fill(root, 'Writable paths (one per line)', 'src/**\nlib/**', true);
    fill(root, 'Protected paths (one per line)', 'tests/**', true);
    fill(root, 'profile-1-javascript: node version', '22.14.0');
    fill(root, 'profile-1-javascript: npm version', '10.9.2');
    fill(
      root,
      'profile-1-javascript: protected test paths (one per line)',
      'tests/quota.mjs',
      true
    );
    fill(root, 'Requirement 1: statement', 'Failure preserves quota');
    fill(root, 'Requirement 1: owner', 'Quota');
    fill(
      root,
      'Requirement 1: case IDs for profile-1-javascript/profile-1-javascript-tests (one per line)',
      'quota-failure',
      true
    );
    fill(root, 'Component name', 'Quota');
    fill(root, 'Component responsibility', 'Own reservations');
    button(root, 'Add component').click();
    fill(root, 'Existing file', 'src/quota.js');
    fill(root, 'Existing symbol', 'reserve');
    fill(root, 'Why reuse or change this code', 'Already owns state');
    button(root, 'Add code reference').click();
    fill(root, 'concurrency Contract', 'Serial');
    fill(root, 'mechanism Pressure', 'Direct function');
    fill(root, 'mechanism Alternatives (one per line)', 'Reuse current function', true);
    fill(root, 'mechanism Cost', 'No new ownership');
    fill(root, 'image Digest', 'sha256:immutable');
    fill(root, 'toolchain Manifest Path', '/host/toolchain.txt');
    fill(root, 'dependency Directory', '/host/offline');
    fill(root, 'Known failing Git revision', 'known-bug');
    fill(root, 'Negative control case IDs (one per line)', 'quota-failure', true);
    const review = [...root.querySelectorAll('label')]
      .find((entry) => entry.textContent?.includes('I reviewed the profile facts'))!
      .querySelector<HTMLInputElement>('input')!;
    review.checked = true;
    review.dispatchEvent(new Event('change', { bubbles: true }));
    assert.equal(button(root, 'Prepare this setup').disabled, false, root.textContent ?? '');
    button(root, 'Prepare this setup').click();
    assert.ok(submitted);
    assert.deepEqual(submitted.writePaths, ['src/**', 'lib/**']);
    assert.deepEqual(submitted.requirements[0].casesByConfiguration, {
      'profile-1-javascript/profile-1-javascript-tests': ['quota-failure'],
    });
    assert.equal(submitted.profiles[0].facts.node_version, '22.14.0');
    assert.equal(submitted.worker.model_id, 'worker');
    assert.equal(submitted.environment.toolchainManifestPath, '/host/toolchain.txt');
    assert.equal('commands' in submitted, false);
  } finally {
    dispose();
  }
});

test('adding another configuration requires its own concrete requirement case mapping', () => {
  const root = container();
  const dispose = render(
    () => (
      <PolyglotPreparation
        model="worker"
        manifests={[{ manifest: 'Cargo.toml', languages: ['rust'] }]}
        busy={false}
        onPrepare={async () => {
          throw new Error('Incomplete setup must not submit');
        }}
      />
    ),
    root
  );
  try {
    button(root, 'Add configuration').click();
    assert.ok(
      root.querySelector(
        '[aria-label="Requirement 1: case IDs for profile-1-rust/profile-1-rust-tests-2 (one per line)"]'
      )
    );
    assert.equal(button(root, 'Prepare this setup').disabled, true);
  } finally {
    dispose();
  }
});
