// ReportFacts.tsx
//
// Under an Autopilot task's report (S09, agent_run/report.rs): the facts
// it was written from, as the IDE recorded them: how the task ended and
// what became of its result (on a server that pushes its results, its
// branch and pull request: H04), the criteria with their verdicts, the
// tests written for it, the last checks, and what it spent.
import { invoke } from '@tauri-apps/api/core';
import { For, Show } from 'solid-js';
import { reportHeadline, reportSpent, type ReportMeta } from '../lib/autopilot';
import { summaryParts } from './StageRecord';

/** The webview doesn't follow a link: the browser opens it. */
function openInBrowser(e: MouseEvent, url: string) {
  e.preventDefault();
  invoke('open_url', { url }).catch((err) => console.error('Open link:', err));
}

export default function ReportFacts(props: { meta: ReportMeta }) {
  const state = () =>
    props.meta.delivered ? 'delivered' : props.meta.partial ? 'partial' : 'undelivered';
  const verdictOf = (i: number) => props.meta.verdicts.find((v) => v.criterion === i + 1);
  return (
    <div class={`chat-verify chat-report ${state()}`} role="note">
      <div class="chat-verify-head">
        <span class="chat-verify-title">{reportHeadline(props.meta)}</span>
      </div>
      <dl class="chat-report-facts">
        <dt>Result</dt>
        <dd>{summaryParts(props.meta.delivery)}</dd>
        <Show when={props.meta.branch}>
          {(branch) => (
            <>
              <dt>Branch</dt>
              <dd>
                <code>{branch()}</code>
              </dd>
            </>
          )}
        </Show>
        <Show when={props.meta.pr}>
          {(pr) => (
            <>
              <dt>Pull request</dt>
              <dd>
                <a href={pr()} onClick={(e) => openInBrowser(e, pr())}>
                  {pr()}
                </a>
              </dd>
            </>
          )}
        </Show>
        <Show when={props.meta.criteria.length}>
          <dt>Criteria</dt>
          <dd>
            <ol class="chat-acceptance-criteria">
              <For each={props.meta.criteria}>
                {(criterion, i) => (
                  <li
                    classList={{
                      met: verdictOf(i())?.satisfied === true,
                      unmet: verdictOf(i())?.satisfied === false,
                    }}
                  >
                    <span class="chat-acceptance-criterion">{criterion}</span>
                    <span class="chat-acceptance-verdict">
                      {verdictOf(i())
                        ? verdictOf(i())!.satisfied
                          ? 'Met'
                          : 'Unmet'
                        : 'Not judged'}
                    </span>
                  </li>
                )}
              </For>
            </ol>
          </dd>
        </Show>
        <Show when={props.meta.tests.length}>
          <dt>Tests written</dt>
          <dd>
            <For each={props.meta.tests}>
              {(file, i) => (
                <>
                  {i() > 0 ? ', ' : ''}
                  <code>{file}</code>
                </>
              )}
            </For>
          </dd>
        </Show>
        <Show when={props.meta.checks.length}>
          <dt>Checks</dt>
          <dd>
            <For each={props.meta.checks}>
              {(check, i) => (
                <>
                  {i() > 0 ? ', ' : ''}
                  <code>{check.command}</code> {check.status}
                </>
              )}
            </For>
          </dd>
        </Show>
        <dt>Spent</dt>
        <dd>{reportSpent(props.meta.spent)}</dd>
      </dl>
      <Show when={props.meta.byIde}>
        <div class="chat-verify-progress">
          The model couldn’t write this report: the IDE wrote it from its records.
        </div>
      </Show>
    </div>
  );
}
