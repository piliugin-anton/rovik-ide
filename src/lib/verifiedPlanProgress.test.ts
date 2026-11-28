import { test } from 'node:test';
import assert from 'node:assert/strict';
import { implementationProgress, preparationActivity } from './verifiedPlanProgress';
import type { ChatMessage } from '../types';
import { implementationRun } from '../test/polyglotPlanFixtures';

test('preparation shows live tool activity and trial diagnostics only for the current request', () => {
  const messages: ChatMessage[] = [
    { id: 'user', role: 'user', content: 'Prepare', meta: { verifiedPreparation: 'session' } },
    {
      id: 'assistant',
      role: 'assistant',
      content: '',
      toolCalls: [{ id: 'submit', name: 'prepare_verified_plan', arguments: '{}' }],
    },
  ];
  assert.match(preparationActivity(messages, 'session')!.activity!, /running trial checks/);
  assert.equal(
    preparationActivity(messages, 'session', { submit: 'Restoring dependencies' })!.activity,
    'Restoring dependencies'
  );
  messages.push({
    id: 'failure',
    role: 'tool',
    tool_call_id: 'submit',
    meta: { outcome: 'failed' },
    content:
      'Error: verification preparation needs corrections.\n' +
      JSON.stringify({
        trial: {
          observations: [
            {
              stage: 'baseline',
              matches: false,
              result: {
                check_id: 'frontend',
                status: 'error',
                diagnostics: [{ message: 'Missing JSX transform' }],
              },
            },
          ],
        },
      }),
  });
  assert.match(
    preparationActivity(messages, 'session')!.failure!,
    /baseline.*frontend.*Missing JSX transform/
  );
  messages.push({
    id: 'pause',
    role: 'assistant',
    content: '',
    meta: { paused: { kind: 'drain', toolCalls: 1 } },
  });
  assert.match(preparationActivity(messages, 'session')!.failure!, /Rovik restarted/);
  messages.push({ id: 'other', role: 'user', content: 'Unrelated' });
  assert.equal(preparationActivity(messages, 'session'), undefined);
});

test('implementation reports the controller phase and failed check evidence', () => {
  const run = { ...implementationRun('running'), state: 'FAST_CHECKS' };
  assert.equal(implementationProgress(run), 'Running fast checks');
  assert.match(
    implementationProgress({
      ...run,
      status: 'failed',
      checks: [
        {
          check_id: 'ui',
          status: 'fail',
          failed_case_ids: ['click'],
          executed_test_count: 1,
          diagnostics: [],
        },
      ],
    }),
    /ui.*fail.*click/
  );
});
