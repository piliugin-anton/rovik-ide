import type { ChatMessage } from '../types';
import type { HarnessRun } from './polyglotHarness';
import { isRequestMessage } from './requests';
import { callFailed, parseArgsLenient, str } from './toolArgs';

type Check = NonNullable<HarnessRun['checks']>[number];

function checkSummary(check: Check) {
  const diagnostic = check.diagnostics?.[0];
  return [check.check_id, check.status, diagnostic?.message || check.failed_case_ids?.[0]]
    .filter(Boolean)
    .join(' · ');
}

function toolFailure(message: ChatMessage) {
  const [headline, payload] = message.content.split('\n', 2);
  if (payload && headline.startsWith('Error: verification preparation needs corrections')) {
    try {
      const trial = JSON.parse(payload).trial?.observations?.find(
        (item: { matches: boolean }) => item.matches === false
      );
      if (typeof trial?.result?.check_id === 'string')
        return [trial.stage.replaceAll('_', ' '), checkSummary(trial.result)].join(' · ');
    } catch {
      // Stored previews can end before the full trial report.
    }
    return headline;
  }
  return message.content.trim() || 'The preparation tool failed.';
}

/** Latest reported activity in this preparation request, scoped to its owning conversation. */
export function preparationActivity(
  messages: readonly ChatMessage[],
  id: string,
  notes?: Record<string, string>
) {
  let start = messages.length - 1;
  while (start >= 0 && !isRequestMessage(messages[start])) start--;
  if (start < 0 || messages[start].meta?.verifiedPreparation !== id) return;
  const results = new Map<string, ChatMessage>();
  let status: string | undefined;
  for (let index = messages.length - 1; index >= start; index--) {
    const message = messages[index];
    const failure =
      message.error || message.meta?.failure?.message || message.meta?.interrupted?.message;
    if (failure) return { failure };
    status ||=
      message.status?.text ||
      (message.role === 'assistant' && !message.toolCalls?.length
        ? message.content.trim().split('\n')[0]?.slice(0, 240)
        : undefined);
    if (message.role === 'tool' && message.tool_call_id) results.set(message.tool_call_id, message);
    const calls = message.toolCalls;
    if (!calls?.length) continue;
    for (const call of calls) {
      const result = results.get(call.id);
      if (result && callFailed(result)) return { failure: toolFailure(result) };
    }
    const call = calls.find((call) => !results.has(call.id)) ?? calls[calls.length - 1];
    const complete = results.has(call.id);
    const path = str(parseArgsLenient(call.arguments).path);
    const labels: Record<string, string> = {
      read: complete ? 'Read' : 'Reading',
      write: complete ? 'Updated' : 'Updating',
      list_dir: complete ? 'Listed' : 'Listing',
      search: complete ? 'Searched' : 'Searching',
      prepare_verified_plan: complete
        ? 'Submitted verification checks'
        : 'Validating the preparation and running trial checks',
      ask_user: complete ? 'Received your answer' : 'Waiting for your answer in this chat',
    };
    const label = labels[call.name] ?? (complete ? 'Finished ' : 'Running ') + call.name;
    return {
      activity:
        (!complete && notes?.[call.id]) || status || [label, path].filter(Boolean).join(' '),
    };
  }
  return { activity: status };
}

const phases: Record<string, string> = {
  INTAKE: 'Checking project scope',
  BASELINE: 'Checking the baseline',
  CONTRACT_READY: 'Preparing the design',
  DESIGN_READY: 'Registering protected checks',
  TESTS_READY: 'Preparing the implementation workspace',
  IMPLEMENT: 'Editing the candidate',
  FAST_CHECKS: 'Running fast checks',
  RISK_CHECKS: 'Running risk checks',
  REVIEW: 'Reviewing the candidate',
  CLEAN_CI: 'Verifying the clean candidate',
  READY_FOR_APPROVAL: 'Checking approvals',
  REPAIR: 'Repairing the candidate',
  REPLAN: 'Revising the design',
};

/** Current controller phase and recorded check evidence, including a failed check's diagnostic. */
export function implementationProgress(run: HarnessRun) {
  const checks = run.checks ?? [];
  if (run.status === 'running') {
    const last = checks[checks.length - 1];
    return [phases[run.state] ?? run.state, last && checkSummary(last)].filter(Boolean).join(' · ');
  }
  const failed = checks
    .slice()
    .reverse()
    .find((check) => ['fail', 'error', 'timeout', 'unsupported'].includes(check.status));
  return [run.reason || run.reasons?.[0], failed && checkSummary(failed)]
    .filter(Boolean)
    .join(' · ');
}
