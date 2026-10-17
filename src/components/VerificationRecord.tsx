// VerificationRecord.tsx
//
// One round of the verification gate in the chat (lib/verification.ts):
// what it concluded, each check with its result, and the output of failed
// checks on demand. Updates live while the checks run.
import { For, Match, Show, Switch } from 'solid-js';
import { CHECK_LABEL } from '../lib/projectProfile';
import { plural } from '../lib/textUtils';
import { liveLabel, liveSubagent } from '../lib/subagents';
import {
  attemptText,
  causeLine,
  describeFailure,
  environmentHeadline,
  progressLine,
  repairHeadline,
  reviewAfterGreenLine,
  triageHeadline,
  type CheckResult,
  type TriageMeta,
  type VerificationMeta,
} from '../lib/verification';
import { SubagentTranscript } from './ToolCallBlock';
import { createSubagentWork, ShowWorkButton } from './SubagentWork';

function headline(meta: VerificationMeta, interrupted: boolean): string {
  if (interrupted) return 'Checks didn’t finish: the run ended while they were running.';
  if (meta.midRun) {
    switch (meta.status) {
      case 'running':
        return 'Checking the workers’ merged changes…';
      case 'passed':
        return 'Checks pass on the workers’ merged changes.';
      case 'failed':
        return 'Checks fail on the workers’ merged changes: sent to the agent, with whose step each error is in.';
      default:
        break;
    }
  }
  switch (meta.status) {
    case 'running':
      return 'Checking the changes…';
    case 'passed':
      return 'Checks passed.';
    case 'failed':
      if (meta.environment) return environmentHeadline(meta.environment);
      if (meta.repair?.status === 'recheck')
        return `Checks failed (${attemptText(meta)}): the workers that own the failures fixed them.`;
      if (meta.repair?.status === 'fixing')
        return `Checks failed (${attemptText(meta)}): the workers that own the failures are fixing them.`;
      return `Checks failed: sent to the agent to fix (${attemptText(meta)}).`;
    case 'gave-up': {
      const after = meta.noProgress
        ? `after ${meta.round} attempts, the last two without progress`
        : `after ${meta.round} attempts`;
      return meta.rolledBack
        ? `The last fix broke something that passed before, so it was rolled back. Checks still fail ${after}: the agent stopped here.`
        : `Checks still fail ${after}. The agent stopped here.`;
    }
    case 'rolled-back':
      if (meta.rolledBack?.kept?.length)
        return `The fix broke something that passed before: the files behind it were put back (${meta.rolledBack.restored.join(', ') || 'none could be'}), the rest of it stays (${meta.rolledBack.kept.join(', ')}), and the checks run again.`;
      return `The fix broke something that passed before, so it was rolled back (${attemptText(meta)}).`;
    case 'flagged':
      return 'Checks pass, but the change weakens tests or what the checks run: review it before relying on it.';
    case 'unresolved':
      return 'The agent finished without changing anything after the failed checks.';
    case 'needs-approval':
      return 'The changes weren’t checked yet: command approval is on.';
    case 'superseded':
      return 'These checks didn’t run: a newer request came after them.';
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

/** The round's failure triage (C03): what it found, or why it was skipped. */
function TriageBlock(props: {
  triage: TriageMeta;
  dialogId: string | null;
  recordId: string;
  live: boolean;
}) {
  const callId = () => `${props.recordId}:triage`;
  const progress = () => liveSubagent(props.dialogId, callId());
  const childId = () => props.triage.childDialogId ?? progress()?.childDialogId ?? null;
  const interrupted = () => props.triage.status === 'running' && !props.live;
  // P5.9: neither a finished triage's live entry nor an unshown transcript stays.
  const work = createSubagentWork({
    dialogId: () => props.dialogId,
    toolCallId: callId,
    childId,
    finishedChildId: () => {
      const id = props.triage.childDialogId;
      return props.triage.status !== 'running' ? id : null;
    },
  });
  return (
    <div class={`chat-verify-triage ${interrupted() ? 'stopped' : props.triage.status}`}>
      <div class="chat-verify-progress">
        {triageHeadline(props.triage, CHECK_LABEL[props.triage.check] ?? 'check', interrupted())}
        <Show when={props.triage.status === 'running' && !interrupted() && progress()}>
          {(p) => <> {liveLabel(p(), false)}</>}
        </Show>
        <Show when={childId() && props.triage.status !== 'running'}>
          {' '}
          <ShowWorkButton work={work} />
        </Show>
      </div>
      <Show when={props.triage.result}>
        {(r) => (
          <div class="review-finding-body">
            <div>
              <span class="review-label">Root cause:</span> {r().rootCause}
            </div>
            <Show when={r().files.length}>
              <div>
                <span class="review-label">Files to fix:</span> {r().files.join('; ')}
              </div>
            </Show>
            <div>
              <span class="review-label">Next step:</span> {r().nextStep}
            </div>
            <Show when={r().confidenceNote}>
              <div>
                <span class="review-label">Confidence:</span> {r().confidence} ({r().confidenceNote}
                )
              </div>
            </Show>
          </div>
        )}
      </Show>
      <Show when={work.shown() && childId()}>
        <SubagentTranscript
          childDialogId={childId()!}
          running={props.triage.status === 'running'}
        />
      </Show>
    </div>
  );
}

export default function VerificationRecord(props: {
  meta: VerificationMeta;
  /** The chat it's in, and this record's id (its triage's events name it). */
  dialogId: string | null;
  recordId: string;
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

      <Show when={props.meta.fromPlan}>
        <div class="chat-verify-progress">
          No checks are set up for this project: these are the commands the plan’s Validation
          section names.
        </div>
      </Show>
      <Show when={props.meta.integrityChecked === false}>
        <div class="chat-verify-progress">
          Whether the change weakens tests couldn’t be checked: part of what the files were before
          the run was lost. Review the changes before relying on them.
        </div>
      </Show>
      <Show when={props.meta.stateRestored === false}>
        <div class="chat-verify-progress">
          The earlier rounds’ state was gone (the app restarted, or it expired): these checks
          started afresh.
        </div>
      </Show>
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
                          : check.status === 'skipped' || check.status === 'slow'
                            ? `${check.status}: ${check.reason}`
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

      <Show when={props.meta.environment?.installs.length ? props.meta.environment : undefined}>
        {(env) => (
          <ul class="chat-verify-checks">
            <For each={env().installs}>
              {(install) => (
                <li class={`chat-verify-check ${install.status}`}>
                  <Show
                    when={install.status === 'failed' && install.output}
                    fallback={
                      <div class="chat-verify-row">
                        <StatusIcon status={install.status} />
                        <span class="chat-verify-kind">Install</span>
                        <code class="chat-verify-command">{install.command}</code>
                        <span class="chat-verify-note">
                          {[install.folder && `in ${install.folder}`, seconds(install.durationMs)]
                            .filter(Boolean)
                            .join(' · ')}
                        </span>
                      </div>
                    }
                  >
                    <details>
                      <summary class="chat-verify-row">
                        <StatusIcon status={install.status} />
                        <span class="chat-verify-kind">Install</span>
                        <code class="chat-verify-command">{install.command}</code>
                        <span class="chat-verify-note">
                          {install.timedOut
                            ? 'timed out'
                            : install.exitCode != null
                              ? `exit ${install.exitCode}`
                              : ''}
                          {install.folder ? ` · in ${install.folder}` : ''}
                        </span>
                      </summary>
                      <pre class="chat-verify-output">{install.output}</pre>
                    </details>
                  </Show>
                </li>
              )}
            </For>
          </ul>
        )}
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
              {rb().kept?.length ? ` · kept: ${rb().kept!.join(', ')}` : ''}
            </summary>
            <Show when={rb().diff}>
              <pre class="chat-verify-output">{rb().diff}</pre>
            </Show>
          </details>
        )}
      </Show>

      <Show when={props.meta.triage}>
        {(t) => (
          <TriageBlock
            triage={t()}
            dialogId={props.dialogId}
            recordId={props.recordId}
            live={props.live}
          />
        )}
      </Show>

      <Show when={props.meta.repair}>
        {(r) => (
          <div class="chat-verify-repair">
            <div class="chat-verify-progress">{repairHeadline(r())}</div>
            <ul class="chat-verify-flags">
              <For each={r().causes}>{(c) => <li>{causeLine(c)}</li>}</For>
            </ul>
          </div>
        )}
      </Show>

      <Show when={props.meta.status === 'gave-up' && props.meta.remaining?.length}>
        <details class="chat-verify-rollback">
          <summary>
            Still failing: {props.meta.remaining!.length}
            {props.meta.attempts?.length
              ? ` · ${plural(props.meta.attempts.length, 'fix attempt')} rolled back`
              : ''}
          </summary>
          <ul class="chat-verify-flags">
            <For each={props.meta.remaining}>{(k) => <li>{describeFailure(k)}</li>}</For>
          </ul>
          <Show when={props.meta.triageSummary}>
            <div>
              <span class="review-label">Last triage:</span> {props.meta.triageSummary}
            </div>
          </Show>
          <For each={props.meta.attempts ?? []}>
            {(rb) => (
              <div>
                <span class="review-label">Rolled back:</span>{' '}
                {rb.broke.map(describeFailure).join(', ')}
                <Show when={rb.diff}>
                  <pre class="chat-verify-output">{rb.diff}</pre>
                </Show>
              </div>
            )}
          </For>
        </details>
      </Show>

      <Show when={reviewAfterGreenLine(props.meta)}>
        {(line) => <div class="chat-verify-progress">{line()}</div>}
      </Show>

      <Show when={props.meta.attribution?.length}>
        <ul class="chat-verify-flags">
          <For each={props.meta.attribution}>
            {(a) => (
              <li>
                {a.location}: {a.owner}
              </li>
            )}
          </For>
        </ul>
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
