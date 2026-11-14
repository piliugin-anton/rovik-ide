import type { ModelProfile } from '../types';
import type { HarnessInspection, HarnessRun } from '../lib/polyglotHarness';
import type { PlanPreparation } from '../lib/planPreparation';
export const planProfile: ModelProfile = {
  id: 'provider::worker',
  label: 'Local worker',
  model: 'worker',
  baseUrl: 'http://127.0.0.1:8080/v1',
  apiKey: 'ephemeral-secret',
  stream: false,
  vision: false,
  pdf: false,
  noTools: false,
  reasoning: { choice: 'off' },
  apiRoute: 'chat_completions',
  harnessRates: { input: 0, cachedInput: 0, output: 0 },
};
export const savedHarnessPlan = {
  path: '.rovik/plans/repair.md',
  content: '# Fix quota\r\n\nPreserve quota on failure.\n',
  digest: 'sha256:exact-plan',
};
export function planPreparation(status: PlanPreparation['status'] = 'ready'): PlanPreparation {
  return {
    preparationId: 'preparation-1',
    status,
    plan: savedHarnessPlan,
    snapshotDigest: 'sha256:source',
    providerBinding: {
      profileId: planProfile.id,
      model: planProfile.model,
      endpoint: `${planProfile.baseUrl}/chat/completions`,
      rates: planProfile.harnessRates!,
    },
    ...(status === 'ready'
      ? {
          review: {
            digest: 'sha256:review',
            label: 'Quota contract',
            summary: 'Preserve quota on failure.',
            setupId: 'setup',
            scope: { writable: ['src/**'], protected: ['tests/**', 'package-lock.json'] },
            checks: [{ id: 'quota-tests', label: 'Quota behavior', configuration: 'node-tests' }],
            requirements: ['remaining >= 0'],
            warnings: [],
            budgets: { model_calls: 16, tokens: 2_000_000, cost_microunits: 10_000_000 },
          },
        }
      : {}),
  };
}
export function implementationRun(status: HarnessRun['status'] = 'blocked'): HarnessRun {
  return {
    runId: 'implementation-1',
    kind: 'implementation',
    planDigest: savedHarnessPlan.digest,
    state: status === 'verified' ? 'ACCEPTED' : status.toUpperCase(),
    status,
    snapshotDigest: 'sha256:source',
    candidateDigest: 'sha256:candidate',
    pendingApprovals: [],
    canResume: status === 'blocked',
    canCancel: status === 'running' || status === 'blocked',
    canRecover: false,
  };
}
export function planInspection(): HarnessInspection {
  const engine = { kind: 'docker' as const, executable: '/usr/bin/docker' };
  return {
    projectId: 'project',
    snapshotDigest: 'sha256:source',
    inventory: {
      units: [
        {
          root: '',
          manifest: 'package.json',
          languages: ['typescript'],
          locks: ['package-lock.json'],
        },
      ],
      unresolved: [],
    },
    untracked: ['src/new.ts'],
    issues: [],
    host: { supported: true, platform: 'linux', engines: [engine] },
    configuration: {
      digest: 'sha256:configuration',
      setupId: 'setup',
      label: 'Quota contract',
      engine,
    },
    setups: [
      {
        id: 'setup',
        label: 'Quota contract',
        contractSummary: 'Failed reservations preserve quota.',
        checks: [],
        configurations: ['ts-tests'],
        invariants: [
          { id: 'quota', statement: 'remaining >= 0', owner: 'Quota', case_ids: ['quota-failure'] },
        ],
      },
    ],
    runs: [],
  };
}
