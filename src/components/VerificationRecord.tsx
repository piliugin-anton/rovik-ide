// VerificationRecord.tsx
//
// One round of the verification gate in the chat (lib/verification.ts):
// what it concluded, each check with its result, and the output of failed
// checks on demand. Updates live while the checks run.
import { For, Match, Show, Switch } from 'solid-js';
import { CHECK_LABEL } from '../lib/projectProfile';
import {
  describeFailure,
  progressLine,
  type CheckResult,
  type VerificationMeta,
} from '../lib/verification';

function headline(meta: VerificationMeta, interrupted: boolean): string {
  if (interrupted) return 'Checks didn’t finish: the run ended while they were running.';
  switch (meta.status) {
    case 'running':
      return 'Checking the changes…';
    case 'passed':
      return 'Checks passed.';
    case 'failed':
      return `Checks failed: sent to the agent to fix (attempt ${meta.round} of ${meta.maxRounds}).`;
    case 'gave-up':
      return meta.rolledBack
        ? `The last fix broke something that passed before, so it was rolled back. Checks still fail after ${meta.round} attempts: the agent stopped here.`
        : `Checks still fail after ${meta.round} attempts. The agent stopped here.`;
    case 'rolled-back':
      return `The fix broke something that passed before, so it was rolled back (attempt ${meta.round} of ${meta.maxRounds}).`;
    case 'flagged':
      return 'Checks pass, but the change weakens tests or what the checks run: review it before relying on it.';
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

/** A check that was waiting or running when the run ended shows as not run. */
function shownStatus(check: CheckResult, interrupted: boolean): CheckResult['status'] {
  return interrupted && (check.status === 'running' || check.status === 'pending')
    ? 'skipped'
    : check.status;
}

export default function VerificationRecord(props: {
  meta: VerificationMeta;
  /** The newest message, nothing running: its buttons are live. */
  latest: boolean;
  /** Checks for this record are running right now. */
  live: boolean;
  onRunChecks: () => void;
  onOpenProjectChecks: () => void;
}) {
  /** Saved as running, but nothing runs it: the app closed mid-check. */
  const interrupted = () => props.meta.status === 'running' && !props.live;
  return (
    <div class={`chat-verify ${interrupted() ? 'stopped' : props.meta.status}`} role="status">
      <div class="chat-verify-head">
        <span class="chat-verify-title">{headline(props.meta, interrupted())}</span>
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
              <li class={`chat-verify-check ${shownStatus(check, interrupted())}`}>
                <Show
                  when={check.status === 'failed' && check.output}
                  fallback={
                    <div class="chat-verify-row">
                      <StatusIcon status={shownStatus(check, interrupted())} />
                      <span class="chat-verify-kind">{CHECK_LABEL[check.kind]}</span>
                      <code class="chat-verify-command">{check.command}</code>
                      <span class="chat-verify-note">
                        {shownStatus(check, interrupted()) !== check.status
                          ? 'didn’t finish'
                          : check.status === 'skipped'
                            ? `skipped: ${check.reason}`
                            : check.status === 'running' && check.note
                              ? check.note
                              : [seconds(check.durationMs), check.note].filter(Boolean).join(' · ')}
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
                        {check.note ? ` · ${check.note}` : ''}
                      </span>
                    </summary>
                    <Show when={check.errorFiles}>
                      {(files) => (
                        <div class="chat-verify-where">
                          <Show when={files().changed.length}>
                            Errors in changed files: {files().changed.join(', ')}.{' '}
                          </Show>
                          <Show when={files().other.length}>
                            In other files: {files().other.join(', ')}.
                          </Show>
                        </div>
                      )}
                    </Show>
                    <pre class="chat-verify-output">{check.output}</pre>
                  </details>
                </Show>
              </li>
            )}
          </For>
        </ul>
      </Show>

      <Show when={props.meta.status !== 'running' && progressLine(props.meta)}>
        {(line) => <div class="chat-verify-progress">{line()}</div>}
      </Show>

      <Show when={props.meta.rolledBack}>
        {(rb) => (
          <details class="chat-verify-rollback">
            <summary>
              Broke: {rb().broke.map(describeFailure).join(', ')}
              {rb().restored.length ? ` · put back: ${rb().restored.join(', ')}` : ''}
              {rb().skipped.length ? ` · left as they were: ${rb().skipped.join(', ')}` : ''}
            </summary>
            <Show when={rb().diff}>
              <pre class="chat-verify-output">{rb().diff}</pre>
            </Show>
          </details>
        )}
      </Show>

      <Show when={props.meta.integrity?.length}>
        <ul class="chat-verify-flags">
          <For each={props.meta.integrity}>{(flag) => <li>{flag}</li>}</For>
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
