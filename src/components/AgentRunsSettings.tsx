// AgentRunsSettings.tsx
//
// Settings → Agent runs: what happens as a chat starts, while the agent
// works on a request (a run) and when it finishes, in every chat on this
// computer. The settings are the permissions' (lib/permissions.ts; the
// backend enforces them with the rest,
// src-tauri/crates/rovik-core/src/agent_run), except the offer of a branch
// to a new chat (lib/chatBranch.ts) and the editor's opening of the files
// the agent edits, which is the editor's (lib/editorSettings.ts). Each
// says what it affects, and when it has no effect: the conflicts rule
// needs isolated chats, so virtual files; the checks need a project with
// checks set up (Project checks).
import { createResource, For, Show } from 'solid-js';
import type { SettingsPage } from '../lib/appSettings';
import {
  TOOL_CALL_CHECKPOINT_CHOICES,
  type AgentPermissions,
  type ChecklistMode,
} from '../lib/permissions';
import { editorSettings, updateEditorSettings } from '../lib/editorSettings';
import { offersChatBranch, setOffersChatBranch } from '../lib/chatBranch';
import { vfsStatus } from '../lib/virtualFiles';
import { countChecksThatRun } from '../lib/projectProfile';
import { plural } from '../lib/textUtils';
import { folderName } from './SettingsMenu';
import { SettingRow, SettingStatus } from './SettingsControls';

interface AgentRunsSettingsProps {
  permissions: AgentPermissions;
  onChange: (permissions: AgentPermissions) => void;
  /** The open project, whose checks "Check the changes" runs; none without one. */
  rootPath: string | null;
  onOpenPage: (page: SettingsPage) => void;
}

const CHECKLIST_EXPLAIN: Record<ChecklistMode, string> = {
  auto: 'The agent keeps a checklist for tasks of three or more steps, and is asked for one once if a task outgrows that without it.',
  always: 'Every request starts with a checklist.',
  off: 'The agent is never asked for a checklist. Implementing a plan still starts with one.',
};

export default function AgentRunsSettings(props: AgentRunsSettingsProps) {
  const p = () => props.permissions;
  const patch = (changes: Partial<AgentPermissions>) => props.onChange({ ...p(), ...changes });

  // Isolated chats need virtual files: without them, chats work in the
  // project folder and the conflicts rule has nothing to apply to. When
  // their status can't be read, it isn't known: nothing is said.
  const [vfs] = createResource(() => vfsStatus().catch(() => undefined));
  const noIsolation = () => vfs.state === 'ready' && vfs() !== undefined && !vfs()!.mountpoint;

  // The open project's checks, as Project checks has them: what would run.
  const [checks] = createResource(() => props.rootPath, countChecksThatRun);
  const folder = () => (props.rootPath ? folderName(props.rootPath) : '');

  const pauseExplain = () =>
    p().toolCallCheckpoint === null
      ? 'The run never pauses for its length alone.'
      : `After every ${p().toolCallCheckpoint} tool calls the run stops and waits for you to continue it.`;

  return (
    <div class="permissions-settings agent-runs-settings">
      <p class="settings-hint">
        What happens as a chat starts, while the agent works on a request and when it finishes, in
        every chat on this computer. While it works and when it finishes, Autopilot’s tasks follow
        their own rules (
        <button class="link-button" onClick={() => props.onOpenPage('autopilot')}>
          Autopilot
        </button>
        ): they never pause on count, always check their changes, and keep a checklist when needed.
      </p>

      <section class="settings-section">
        <div class="settings-section-title">When a chat starts</div>
        <label class="setting-checkbox">
          <input
            type="checkbox"
            checked={offersChatBranch()}
            onChange={(e) => setOffersChatBranch(e.currentTarget.checked)}
          />
          <span>
            Offer a new chat a branch of its own
            <span class="setting-checkbox-sub">
              When a new chat’s first message is sent while the project is on its main branch, the
              agent waits while you’re asked whether to create a branch for the chat’s work and
              switch to it, under a name made up for it. “Don’t ask again” there turns this off.
            </span>
          </span>
        </label>
      </section>

      <section class="settings-section">
        <div class="settings-section-title">While it works</div>
        <SettingRow label="Pause every">
          <select
            class="settings-select"
            aria-label="Pause every"
            value={String(p().toolCallCheckpoint ?? 'never')}
            onChange={(e) => {
              const value = e.currentTarget.value;
              patch({ toolCallCheckpoint: value === 'never' ? null : Number(value) });
            }}
          >
            <For each={TOOL_CALL_CHECKPOINT_CHOICES}>
              {(n) => (
                <option value={String(n ?? 'never')}>
                  {n === null ? 'Never' : `${n} tool calls`}
                </option>
              )}
            </For>
          </select>
        </SettingRow>
        <p class="settings-hint setting-explain">
          {pauseExplain()} Whatever this is set to, a run pauses when it seems stuck: the same call
          again and again, failure after failure, an edit undone.
        </p>

        <SettingRow label="Checklist">
          <select
            class="settings-select"
            aria-label="Checklist"
            value={p().checklist}
            onChange={(e) => patch({ checklist: e.currentTarget.value as ChecklistMode })}
          >
            <option value="auto">When needed</option>
            <option value="always">Always</option>
            <option value="off">Off</option>
          </select>
        </SettingRow>
        <p class="settings-hint setting-explain">
          {CHECKLIST_EXPLAIN[p().checklist]} The checklist shows above the chat while the agent
          works through it.
        </p>
      </section>

      <section class="settings-section">
        <div class="settings-section-title">When it finishes</div>
        <label class="setting-checkbox">
          <input
            type="checkbox"
            checked={p().verifyChanges}
            onChange={(e) => patch({ verifyChanges: e.currentTarget.checked })}
          />
          <span>
            Check the changes
            <span class="setting-checkbox-sub">
              After a run in which the agent changed files, the project’s checks run: type check,
              lint, tests, build. A failure goes back to the agent to fix, up to 3 times. A check
              that isn’t among the allowed commands is skipped, and with command approval by you,
              you start them yourself.
            </span>
          </span>
        </label>
        <Show
          when={props.rootPath}
          fallback={
            <SettingStatus>
              No project is open. The checks are set per project, in Project checks, once one is.
            </SettingStatus>
          }
        >
          <Show
            when={checks.state === 'ready'}
            fallback={
              <SettingStatus warning={checks.state === 'errored'}>
                {checks.state === 'errored'
                  ? `Couldn’t read ${folder()}’s checks.`
                  : `Looking at ${folder()}’s checks…`}
              </SettingStatus>
            }
          >
            <SettingStatus warning={!p().verifyChanges || checks() === 0}>
              {!p().verifyChanges
                ? `Off: ${folder()}’s checks don’t run after the agent’s changes.`
                : checks() === 0
                  ? `Nothing to run: no checks are set up for ${folder()}.`
                  : `${plural(checks() ?? 0, 'check')} set up for ${folder()} run after a change.`}{' '}
              <button class="link-button" onClick={() => props.onOpenPage('project')}>
                Project checks
              </button>
            </SettingStatus>
          </Show>
        </Show>
      </section>

      <section class="settings-section">
        <div class="settings-section-title">Isolated chats</div>
        <SettingRow label="Conflicts">
          <select
            class="settings-select"
            aria-label="Conflicts"
            value={p().chatResolvesConflicts ? 'chat' : 'me'}
            onChange={(e) => patch({ chatResolvesConflicts: e.currentTarget.value === 'chat' })}
          >
            <option value="chat">The running chat resolves them</option>
            <option value="me">Ask me</option>
          </select>
        </SettingRow>
        <p class="settings-hint setting-explain">
          Each chat works on its own copy of your changes. When a running chat’s changes conflict
          with the project’s (another chat landed, or you edited the same lines), the chat gets
          conflict markers in its files and combines both before it finishes. Ask me: the conflicts
          wait above the message box for you to resolve. A chat that isn’t running always waits for
          you.
        </p>
        <Show when={noIsolation()}>
          <SettingStatus warning>
            No effect now: virtual files aren’t mounted on this computer, so chats work in the
            project folder directly and don’t conflict this way.{' '}
            <button class="link-button" onClick={() => props.onOpenPage('vfs')}>
              Virtual files
            </button>
          </SettingStatus>
        </Show>
      </section>

      <section class="settings-section">
        <div class="settings-section-title">In the editor</div>
        <label class="setting-checkbox">
          <input
            type="checkbox"
            checked={editorSettings().openFilesAfterAgentEdits}
            onChange={(e) =>
              updateEditorSettings({ openFilesAfterAgentEdits: e.currentTarget.checked })
            }
          />
          <span>
            Open the files the agent edits
            <span class="setting-checkbox-sub">
              A file the agent writes or edits opens in a tab so its changes can be reviewed: in the
              background while you work in another file, in front when nothing else is open. “Review
              in editor” on an edit always opens it.
            </span>
          </span>
        </label>
      </section>
    </div>
  );
}
