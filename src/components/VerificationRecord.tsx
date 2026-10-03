// VerificationRecord.tsx
//
// One round of the verification gate in the chat (lib/verification.ts):
// what it concluded, each check with its result, and the output of failed
// checks on demand. Updates live while the checks run.
import { For, Match, Show, Switch } from 'solid-js';
import { CHECK_LABEL } from '../lib/projectProfile';
import type { CheckResult, VerificationMeta } from '../lib/verification';

function headline(meta: VerificationMeta): string {
  switch (meta.status) {
    case 'running':
      return 'Checking the changes…';
    case 'passed':
      return 'Checks passed.';
    case 'failed':
      return `Checks failed: sent to the agent to fix (attempt ${meta.round} of ${meta.maxRounds}).`;
    case 'gave-up':
      return `Checks still fail after ${meta.round} attempts. The agent stopped here.`;
    case 'unresolved':
      return 'The agent finished without changing anything after the failed checks.';
    case 'needs-approval':
      return 'The changes weren’t checked yet: command approval is on.';
    case 'unverified':
      return `The changes weren’t checked: ${meta.reason ?? 'no check could run'}.`;
    case 'stopped':
      return 'Checks stopped.';
  }
}

function seconds(ms?: number): string {
  if (ms === undefined) return '';
  return ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(ms < 10_000 ? 1 : 0)} s`;
}

function StatusIcon(props: { status: CheckResult['status'] }) {
  const common = {
    width: 12,
    height: 12,
    viewBox: '0 0 16 16',
    fill: 'none',
    stroke: 'currentColor',
    'stroke-width': 1.8,
    'stroke-linecap': 'round' as const,
    'stroke-linejoin': 'round' as const,
    'aria-hidden': true,
  };
  return (
    <Switch
      fallback={
        <svg {...common}>
          <path d="M4.5 8h7" />
        </svg>
      }
    >
      <Match when={props.status === 'passed'}>
        <svg {...common}>
          <path d="M3.5 8.5 6.5 11.5 12.5 4.5" />
        </svg>
      </Match>
      <Match when={props.status === 'failed'}>
        <svg {...common}>
          <path d="M4.5 4.5l7 7M11.5 4.5l-7 7" />
        </svg>
      </Match>
      <Match when={props.status === 'running'}>
        <svg {...common} class="verify-spin">
          <path d="M13 8a5 5 0 1 1-5-5" />
        </svg>
      </Match>
    </Switch>
  );
}

export default function VerificationRecord(props: {
  meta: VerificationMeta;
  /** The newest message, nothing running: its buttons are live. */
  latest: boolean;
  onRunChecks: () => void;
  onOpenProjectChecks: () => void;
}) {
  return (
    <div class={`chat-verify ${props.meta.status}`} role="status">
      <div class="chat-verify-head">
        <span class="chat-verify-title">{headline(props.meta)}</span>
        <Show when={props.latest && props.meta.status === 'needs-approval'}>
          <button type="button" class="chat-action-button" onClick={() => props.onRunChecks()}>
            Run checks
          </button>
        </Show>
        <Show when={props.meta.status === 'unverified'}>
          <button type="button" class="link-button" onClick={() => props.onOpenProjectChecks()}>
            Set up checks
          </button>
        </Show>
      </div>

      <Show when={props.meta.checks.length > 0}>
        <ul class="chat-verify-checks">
          <For each={props.meta.checks}>
            {(check) => (
              <li class={`chat-verify-check ${check.status}`}>
                <Show
                  when={check.status === 'failed' && check.output}
                  fallback={
                    <div class="chat-verify-row">
                      <StatusIcon status={check.status} />
                      <span class="chat-verify-kind">{CHECK_LABEL[check.kind]}</span>
                      <code class="chat-verify-command">{check.command}</code>
                      <span class="chat-verify-note">
                        {check.status === 'skipped'
                          ? `skipped: ${check.reason}`
                          : seconds(check.durationMs)}
                      </span>
                    </div>
                  }
                >
                  <details>
                    <summary class="chat-verify-row">
                      <StatusIcon status={check.status} />
                      <span class="chat-verify-kind">{CHECK_LABEL[check.kind]}</span>
                      <code class="chat-verify-command">{check.command}</code>
                      <span class="chat-verify-note">
                        {check.timedOut
                          ? 'timed out'
                          : check.exitCode != null
                            ? `exit ${check.exitCode}`
                            : ''}
                        {check.durationMs !== undefined ? `, ${seconds(check.durationMs)}` : ''}
                      </span>
                    </summary>
                    <pre class="chat-verify-output">{check.output}</pre>
                  </details>
                </Show>
              </li>
            )}
          </For>
        </ul>
      </Show>

      <Show when={props.meta.changedFiles.length > 0 && props.meta.status !== 'running'}>
        <div class="chat-verify-files" title={props.meta.changedFiles.join('\n')}>
          Changed: {props.meta.changedFiles.join(', ')}
        </div>
      </Show>
    </div>
  );
}
