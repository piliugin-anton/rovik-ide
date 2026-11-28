import { For, Show, createMemo } from 'solid-js';
import type { HarnessRun } from '../lib/polyglotHarness';
import { plural } from '../lib/textUtils';

const decisions: Record<string, { name: string; prompt: string }> = {
  'domain-contract': {
    name: 'requirements',
    prompt: 'Check that the requirements describe the behavior you want.',
  },
  'baseline-plan': {
    name: 'starting checks',
    prompt:
      'Check which tests establish the project’s starting point and which existing failures are expected.',
  },
  design: {
    name: 'implementation design',
    prompt: 'Check the responsibilities and proposed changes to public interfaces.',
  },
  oracle: {
    name: 'protected tests',
    prompt: 'Check that the protected tests would catch a change that breaks your requirements.',
  },
  delivery: {
    name: 'delivery destination',
    prompt: 'Check which checkout will receive the verified changes.',
  },
  'design-review': {
    name: 'proposed design',
    prompt: 'Check whether the proposed changes implement the saved plan.',
  },
};

/** Name the decision using the language of the plan. */
export function approvalDecision(type: string) {
  return (
    decisions[type] ?? {
      name: type.replace(/[-_]/g, ' '),
      prompt: 'Inspect the proposed decision before approving it.',
    }
  );
}

/** Explain the next decision in the saved-plan workflow. */
export function verificationMessage(run: HarnessRun, status: string) {
  if (status === 'stale')
    return 'The project has changed. Prepare the plan again to check the current files.';
  if (status === 'applied') return 'The changes were applied on the execution host.';
  if (status === 'failed') return 'Resolve the reported problem, then prepare the plan again.';
  if (run.canRecover) return 'Verification was interrupted. Recover the run before continuing.';
  if (run.pendingApprovals.some((item) => item.approvalType === 'design-review'))
    return 'Review the proposed changes against your plan. Approve the design to let verification continue.';
  if (run.pendingApprovals.length) return 'Verification is waiting for you to review a decision.';
  if (run.canResume) return 'Verification paused. Resume when you are ready to continue.';
  return 'Verification stopped before the changes could be confirmed.';
}

const checkOutcomes: Record<string, string> = {
  fail: 'failed',
  error: 'could not run',
  timeout: 'timed out',
  unsupported: 'is unavailable',
};

/** A short result for the decision at hand. Full evidence is available in the exported review. */
export default function VerificationSummary(props: { run: HarnessRun; status: string }) {
  const checks = () => props.run.checks ?? [];
  const failed = createMemo(() => checks().filter((check) => checkOutcomes[check.status]));
  const passed = () => checks().filter((check) => check.status === 'pass').length;
  const reason = () => props.run.reason || props.run.reasons?.[0];
  // Large native tool errors can contain the entire compiler log after the first line.
  const brief = (text: string) => {
    const line =
      text
        .trim()
        .split('\n')
        .find((line) => line.trim()) || '';
    return line.length > 300 ? line.slice(0, 297) + '…' : line;
  };
  return (
    <div class="verification-summary">
      <p>{verificationMessage(props.run, props.status)}</p>
      <Show when={reason() && (!props.run.pendingApprovals.length || failed().length > 0)}>
        <p class="settings-hint">{brief(reason()!)}</p>
      </Show>
      <Show when={checks().length}>
        <p class="settings-hint">
          {passed()} of {plural(checks().length, 'check')} passed.
        </p>
        <For each={failed().slice(0, 3)}>
          {(check) => {
            const diagnostic = check.diagnostics?.[0];
            return (
              <p>
                <strong>
                  {check.check_id} {checkOutcomes[check.status]}.
                </strong>{' '}
                {(diagnostic &&
                  [
                    diagnostic.file && [diagnostic.file, diagnostic.line].filter(Boolean).join(':'),
                    brief(diagnostic.message),
                  ]
                    .filter(Boolean)
                    .join(' — ')) ||
                  check.failed_case_ids?.[0]}
              </p>
            );
          }}
        </For>
        <Show when={failed().length > 3}>
          <p class="settings-hint">
            {plural(failed().length - 3, 'additional check')} with a problem.
          </p>
        </Show>
      </Show>
    </div>
  );
}
