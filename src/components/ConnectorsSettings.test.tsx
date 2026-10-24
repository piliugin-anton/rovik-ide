// Component test: Settings → Connectors' form, laid out the same for every
// kind (ConnectorsSettings.tsx): its name; the Connection section, with
// the kind's sign-in fields before the test and Set up; the kind's own
// options (connectorFields.ts); its models.
// Run with: npm test
import { container } from '../test/dom';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { render } from 'solid-js/web';
import ConnectorsSettings from './ConnectorsSettings';
import { newConnector } from '../lib/connectors';

// No cloud hosts to test on; nothing else is asked of the app.
(window as any).__TAURI_INTERNALS__ = { invoke: async () => ({ hosts: [] }) };

/** What one of the form's parts is: a section by its title, a field by its label. */
function partName(el: Element): string {
  if (el.tagName === 'SECTION')
    return el.querySelector('.settings-section-title, summary')?.textContent ?? 'section';
  if (el.tagName === 'LABEL') return el.firstChild?.textContent?.trim() ?? 'label';
  return el.classList[0] ?? el.tagName;
}

test('a connector’s form: its name, how it connects, its kind’s options, its models', () => {
  const claude = { ...newConnector('claude-code'), id: 'c1' };
  const root = container();
  const dispose = render(
    () => <ConnectorsSettings providers={[claude]} onChange={() => {}} />,
    root
  );
  const edit = [...root.querySelectorAll('button')].find((b) => b.textContent === 'Edit');
  (edit as HTMLButtonElement).click();

  const form = root.querySelector('.connector-form')!;
  assert.deepEqual([...form.children].map(partName), [
    'Name',
    'Connection',
    'How chats use it',
    'Permissions',
    'Limits',
    'Advanced',
    'provider-models',
    'profile-form-actions',
  ]);
  // How it signs in comes first in Connection, then the test of it.
  const connection = form.querySelector('section')!;
  assert.deepEqual([...connection.children].slice(0, 4).map(partName), [
    'settings-section-title',
    'setting-row',
    'settings-hint',
    'provider-test-row',
  ]);
  assert.equal(connection.querySelector('.setting-row-label')?.textContent, 'Signs in with');
  dispose();
});
