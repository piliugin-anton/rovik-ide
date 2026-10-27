// GitPanel.tsx
//
// The window's Git panel (D10): a section per repository the window's
// folders are in (GitRepoSection.tsx), one for each folder in none, and the
// commit message with its button, which are the window's. A project's panel
// is one section with no title, and works as it always has: its commit is
// that repository's (`git_commit`), then pushed, or its branch published.
// With several, one message commits every repository included in it, each
// on the branch it's on (D11, `git_commit_many`): a check or a hook stops it
// before any commit is made; a commit that fails after others keeps the
// message, and nothing is pushed until every one of them is committed.
import {
  createEffect,
  createMemo,
  createSignal,
  For,
  on,
  onCleanup,
  onMount,
  Show,
  splitProps,
} from 'solid-js';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { GitCommitOutcome, GitRepos, GitStatus } from '../types';
import { confirmDialog } from './ConfirmDialog';
import { SKIP_HINTS, skippableHook } from '../lib/gitHooks';
import {
  COMMIT_LABELS,
  commitAction,
  commitActionAcross,
  includable,
  partialText,
  publishHint,
  publishHintAcross,
  publishQuestion,
  publishQuestionAcross,
  stoppedText,
  unpublished,
} from '../lib/gitPublish';
import {
  GitInlineError,
  GitRepoSection,
  type GitPanelEvents,
  type GitRepoHandle,
  type GitSectionSource,
  type SkipOffer,
} from './GitRepoSection';
import { IconAlert, IconArrowUp, IconCheck, IconRefresh, IconSpinner } from './GitIcons';

/** Commit messages being written, by the window's session (`sessionKey`):
 *  the panel goes while another sidebar tab is shown. */
const commitDrafts = new Map<string, string>();

/** The branches whose publishing the user put off after a commit ("Not now"),
 *  by repository and branch: not asked about again while the app runs.
 *  Publish still does it. */
const publishPutOff = new Set<string>();

/** The repositories a commit message is in while another of its commit
 *  failed, by session: they're pushed once every one is committed. */
const heldPushes = new Map<string, Set<string>>();

interface GitPanelProps extends GitPanelEvents {
  /** The window's folders that are there: a project's one, or a workspace's. */
  folders: readonly { path: string; name: string }[];
  /** The window's session (`sessionKey`): its commit message is kept by it. */
  session: string;
}

/** A section in the next commit, with its status. */
interface Included {
  source: GitSectionSource;
  handle: GitRepoHandle;
  status: GitStatus;
}

const errorText = (e: unknown) => (e instanceof Error ? e.message : String(e));

export function GitPanel(props: GitPanelProps) {
  const [events] = splitProps(props, [
    'onOpenFile',
    'onOpenStashFile',
    'onOpenCommitFile',
    'onFileReverted',
    'onChanged',
    'onStashChanged',
    'onFilesChangedOnDisk',
    'onOpenPath',
  ]);

  // The repositories the folders are in: read again as the folders change,
  // and when one becomes a repository of its own or stops being one.
  const [repos, setRepos] = createSignal<GitRepos | null>(null);
  const [reposError, setReposError] = createSignal<string | null>(null);
  let reads = 0;
  const readRepos = async () => {
    const read = ++reads;
    try {
      const found = await invoke<GitRepos>('git_repos', {
        folders: props.folders.map(({ path, name }) => ({ path, name })),
      });
      if (read !== reads) return;
      setRepos(found);
      setReposError(null);
    } catch (e) {
      if (read !== reads) return;
      setRepos(null);
      setReposError(errorText(e));
    }
  };
  createEffect(
    on(
      () => props.folders.map((folder) => `${folder.path}\n${folder.name}`).join('\0'),
      () => void readRepos()
    )
  );
  onMount(() => {
    let stop: (() => void) | undefined;
    let disposed = false;
    void listen<{ folder: string; repositories: boolean }>('git-changed', ({ payload }) => {
      if (payload.repositories) void readRepos();
    }).then((unlisten) => {
      if (disposed) unlisten();
      else stop = unlisten;
    });
    onCleanup(() => {
      disposed = true;
      stop?.();
    });
  });

  /** The sections, in the window's order of their first folders. */
  const sources = createMemo<GitSectionSource[]>(() => {
    const found = repos();
    if (!found) return [];
    const order = (path: string) => props.folders.findIndex((folder) => folder.path === path);
    const all: GitSectionSource[] = [
      ...found.repos.map((repo) => ({
        key: repo.key,
        path: repo.folders[0],
        scopes: repo.scopes,
        name: repo.name,
        folders: repo.folders,
      })),
      ...found.without.map((folder) => ({
        key: `folder:${folder.path}`,
        path: folder.path,
        name: folder.name,
        folders: [folder.path],
      })),
    ];
    return all.sort((a, b) => order(a.path) - order(b.path));
  });
  // A section stays as the list is read again: its history, its menus and
  // its scroll position with it.
  const keys = createMemo(() => sources().map((source) => source.key), [], {
    equals: (a, b) => a.length === b.length && a.every((key, i) => key === b[i]),
  });
  const sourceOf = (key: string) => sources().find((source) => source.key === key);
  const several = () => keys().length > 1;

  const [handles, setHandles] = createSignal<Record<string, GitRepoHandle>>({});
  /** Where the section for `key` hands in its handle, and takes it back. */
  const handleFor = (key: string) => {
    let mine: GitRepoHandle | null = null;
    return (handle: GitRepoHandle | null) =>
      setHandles((all) => {
        const next = { ...all };
        if (handle) next[key] = mine = handle;
        else if (next[key] === mine) delete next[key];
        return next;
      });
  };
  /** The only section's handle: one repository, or one folder in none. */
  const only = () => (several() ? undefined : handles()[keys()[0]]);
  const anyBusy = () => keys().some((key) => handles()[key]?.busy());

  // Kept by session while the panel is away (another sidebar tab shown).
  const [message, setMessageSignal] = createSignal(commitDrafts.get(props.session) ?? '');
  const setMessage = (text: string) => {
    setMessageSignal(text);
    if (text) commitDrafts.set(props.session, text);
    else commitDrafts.delete(props.session);
  };
  // Another project or workspace opened: its own message, if one was being written.
  createEffect(
    on(
      () => props.session,
      (session) => setMessageSignal(commitDrafts.get(session) ?? ''),
      { defer: true }
    )
  );

  /** The repositories the user left out of the commit (their box unticked). */
  const [excluded, setExcluded] = createSignal<ReadonlySet<string>>(new Set());
  const include = (key: string, ticked: boolean) =>
    setExcluded((left) => {
      const next = new Set(left);
      if (ticked) next.delete(key);
      else next.add(key);
      return next;
    });
  /** The sections in the next commit (with several): ticked, with nothing in
   *  the way, and something to commit or push. */
  const included = createMemo<Included[]>(() =>
    keys().flatMap((key) => {
      const handle = handles()[key];
      const source = sourceOf(key);
      const status = handle?.status();
      if (!handle || !source || !status || excluded().has(key) || !includable(status)) return [];
      return [{ source, handle, status }];
    })
  );

  /** A branch's publishing put off, by repository and branch. */
  const putOffKey = (source: GitSectionSource, branch: string) => `${source.key}\n${branch}`;

  // -------------------------------------------------------------------------
  // One repository: the commit a project's panel always made.
  // -------------------------------------------------------------------------

  /** `noVerify`: the commit without the hooks that check (a retry after
   *  one stopped it); a push that follows still runs pre-push. */
  const commitOne = (source: GitSectionSource, section: GitRepoHandle, noVerify = false) =>
    section.run(async () => {
      const st = section.status();
      if (!st) return;
      const msg = message().trim();
      const hasChanges = st.files.length > 0;

      // Retry / push-only: no message but commits waiting to be pushed
      // (e.g. a previous push failed with an SSH/network error), or a
      // branch to publish. There is nothing to commit, so just push.
      if (!msg || !hasChanges) {
        if (st.ahead <= 0 && !unpublished(st)) return;
        try {
          await section.push();
        } finally {
          await section.refreshStatus();
          section.refreshHistory();
        }
        return;
      }

      try {
        await invoke('git_commit', {
          path: source.path,
          message: msg,
          noVerify,
          scopes: source.scopes,
        });
      } catch (e) {
        // A git hook that stopped the commit may have changed files first.
        await section.refreshStatus();
        section.offerSkip(e, 'Commit without hooks', SKIP_HINTS.commit, () =>
          commitOne(source, section, true)
        );
        throw e;
      } finally {
        // A pre-commit hook may rewrite files (a formatter's fixes).
        props.onFilesChangedOnDisk?.();
      }
      setMessage('');

      const updated = await section.refreshStatus();
      try {
        if (updated && updated.ahead > 0) await section.push();
        else if (unpublished(updated)) await publishAfterCommit(source, section, updated);
      } finally {
        await section.refreshStatus();
        section.refreshHistory();
      }
    });

  /** A commit on a branch with no remote branch yet wasn't pushed: asks to
   *  publish the branch, whose push makes its remote branch and tracks it. */
  const publishAfterCommit = async (
    source: GitSectionSource,
    section: GitRepoHandle,
    st: GitStatus
  ) => {
    const key = putOffKey(source, st.branch);
    if (publishPutOff.has(key)) return;
    if (!(await confirmDialog(publishQuestion(st)))) {
      publishPutOff.add(key);
      return;
    }
    await section.push();
  };

  // -------------------------------------------------------------------------
  // Several repositories: one message, each committed on its own branch.
  // -------------------------------------------------------------------------

  const [committing, setCommitting] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [skipOffer, setSkipOffer] = createSignal<SkipOffer | null>(null);

  /** Runs the panel's own action across repositories: every section waits,
   *  and what goes wrong shows above the commit bar. */
  const across = async (fn: () => Promise<void>) => {
    setCommitting(true);
    setError(null);
    setSkipOffer(null);
    try {
      await fn();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setCommitting(false);
    }
  };

  const heldNow = () => {
    let held = heldPushes.get(props.session);
    if (!held) heldPushes.set(props.session, (held = new Set()));
    return held;
  };

  /** Pushes a repository in its own section: what goes wrong shows there,
   *  and the others are still pushed. */
  const pushIn = (section: GitRepoHandle) =>
    section.run(async () => {
      try {
        await section.push();
      } finally {
        await section.refreshStatus();
        section.refreshHistory();
      }
    });

  const pushEach = async (list: readonly Included[]) => {
    for (const one of list) {
      await pushIn(one.handle);
      heldNow().delete(one.source.key);
    }
  };

  /** One question for every branch the commit left with no remote branch. */
  const publishAcross = async (list: readonly Included[]) => {
    const asked = list.filter(
      (one) => !publishPutOff.has(putOffKey(one.source, one.status.branch))
    );
    if (asked.length === 0) return;
    const named = asked.map((one) => ({ name: one.source.name, status: one.status }));
    if (!(await confirmDialog(publishQuestionAcross(named)))) {
      for (const one of asked) publishPutOff.add(putOffKey(one.source, one.status.branch));
      return;
    }
    for (const one of asked) await pushIn(one.handle);
  };

  const commitAcross = async (noVerify: boolean): Promise<void> => {
    const list = included();
    const msg = message().trim();
    const action = commitActionAcross(
      list.map((one) => one.status),
      msg
    );
    if (action === 'push') return pushEach(list.filter((one) => one.status.ahead > 0));
    if (action === 'publish') return pushEach(list.filter((one) => unpublished(one.status)));
    const targets = list.filter((one) => one.status.files.length > 0);
    if (!msg || targets.length === 0) return;

    let outcome: GitCommitOutcome;
    try {
      outcome = await invoke<GitCommitOutcome>('git_commit_many', {
        repos: targets.map((one) => ({ path: one.source.path, scopes: one.source.scopes ?? [] })),
        message: msg,
        noVerify,
      });
    } finally {
      // A pre-commit hook may rewrite files (a formatter's fixes), and what
      // was committed is gone from the changes.
      props.onFilesChangedOnDisk?.();
      for (const one of targets) await one.handle.refreshStatus();
    }
    const named = (path: string) =>
      targets.find((one) => one.source.path === path)?.source.name ?? path;
    const problems = (list: { path: string; error: string }[]) =>
      list.map((problem) => ({ name: named(problem.path), error: problem.error }));

    if (outcome.result === 'stopped') {
      const text = stoppedText(problems(outcome.problems));
      if (outcome.problems.some((problem) => skippableHook(problem.error))) {
        setSkipOffer({
          error: text,
          label: 'Commit without hooks',
          hint: SKIP_HINTS.commit,
          run: () => across(() => commitAcross(true)),
        });
      }
      throw new Error(text);
    }

    const held = heldNow();
    for (const one of targets) {
      if (outcome.committed.includes(one.source.path)) {
        held.add(one.source.key);
        one.handle.refreshHistory();
      }
    }
    if (outcome.failed.length > 0) {
      // The message stays for a retry, and nothing is pushed until every
      // repository it's for has it.
      const done = keys().flatMap((key) => (held.has(key) ? [sourceOf(key)?.name ?? key] : []));
      throw new Error(partialText(done, problems(outcome.failed)));
    }

    setMessage('');
    heldPushes.delete(props.session);
    // Every repository this message is in is pushed, as a project's commit
    // is; one that was only ahead before it isn't.
    const toPublish: Included[] = [];
    for (const key of keys().filter((key) => held.has(key))) {
      const handle = handles()[key];
      const source = sourceOf(key);
      const status = handle?.status();
      if (!handle || !source || !status) continue;
      if (status.ahead > 0) await pushIn(handle);
      else if (unpublished(status)) toPublish.push({ source, handle, status });
    }
    await publishAcross(toPublish);
  };

  // -------------------------------------------------------------------------
  // The commit bar
  // -------------------------------------------------------------------------

  const commit = (noVerify = false) => {
    const section = only();
    const source = sourceOf(keys()[0]);
    if (section && source) return commitOne(source, section, noVerify);
    return across(() => commitAcross(noVerify));
  };

  const action = createMemo(() => {
    const section = only();
    if (section) {
      const s = section.status();
      return s ? commitAction(s, message()) : 'commit';
    }
    return commitActionAcross(
      included().map((one) => one.status),
      message()
    );
  });

  // Push-only retry must stay enabled with an empty message: after a commit
  // succeeds but its auto-push fails (e.g. SSH/network error), `ahead > 0`
  // with a clean tree and a cleared message is exactly the state we are in —
  // requiring a message here would leave the Push button disabled forever,
  // even after a restart.
  const canCommit = createMemo(() => {
    const section = only();
    if (section) {
      const s = section.status();
      if (!s || section.busy()) return false;
      // A merge / rebase in progress is finished from its banner, not here.
      if (s.operation) return false;
      // Push, or Publish a branch with no remote branch yet (lib/gitPublish.ts).
      if (action() !== 'commit') return true;
      return !!message().trim() && s.files.length > 0;
    }
    if (committing() || anyBusy()) return false;
    if (action() !== 'commit') return true;
    return !!message().trim() && included().some((one) => one.status.files.length > 0);
  });

  /** The commit bar shows once a repository's status is read. */
  const barShown = () => {
    const section = only();
    if (section) return !!section.status();
    return keys().some((key) => !!handles()[key]?.status());
  };
  /** The commit bar waits: the only section's action, or the panel's commit. */
  const barBusy = () => only()?.busy() ?? committing();
  /** A merge or a rebase of the only repository, finished from its banner. */
  const operation = () => only()?.status()?.operation ?? null;
  const publishTitle = () => {
    if (action() !== 'publish') return undefined;
    const section = only();
    if (section) {
      const s = section.status();
      return s ? publishHint(s) : undefined;
    }
    const branches = included().filter((one) => unpublished(one.status));
    return publishHintAcross(
      branches.map((one) => ({ name: one.source.name, status: one.status }))
    );
  };

  return (
    <div class="git-panel">
      <Show when={!repos() && !reposError()}>
        <div class="git-panel-loading empty-state">
          <IconSpinner size={16} />
          <span>Loading repository...</span>
        </div>
      </Show>

      <Show when={reposError()}>
        {(text) => (
          <div class="git-panel-empty empty-state git-panel-error">
            <IconAlert />
            <p>{text()}</p>
            <button class="icon-button" onClick={() => void readRepos()}>
              <IconRefresh />
              <span>Retry</span>
            </button>
          </div>
        )}
      </Show>

      <div class="git-repo-sections" classList={{ 'git-repo-sections-several': several() }}>
        <For each={keys()}>
          {(key) => {
            // The section's source as the list is read again; the last one
            // while the section goes.
            let last = sourceOf(key)!;
            const source = () => (last = sourceOf(key) ?? last);
            return (
              <GitRepoSection
                {...events}
                source={source()}
                several={several()}
                included={included().some((one) => one.source.key === key)}
                onInclude={(ticked) => include(key, ticked)}
                locked={committing()}
                onRepositoriesChanged={() => void readRepos()}
                handle={handleFor(key)}
              />
            );
          }}
        </For>
      </div>

      <Show when={several() && error()}>
        {(text) => (
          <GitInlineError error={text()} offer={skipOffer()} disabled={committing() || anyBusy()} />
        )}
      </Show>

      <Show when={barShown()}>
        <div class="git-commit-bar">
          <input
            class="tree-row-input git-commit-input"
            placeholder={
              operation() ? `Finish or abort the ${operation()!.kind} first` : 'Commit message'
            }
            value={message()}
            onInput={(e) => setMessage(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && (e.ctrlKey || e.metaKey) && canCommit()) {
                void commit();
              }
            }}
            disabled={barBusy() || !!operation()}
          />
          <button
            class="git-commit-button primary icon-button"
            disabled={!canCommit()}
            onClick={() => void commit()}
            title={publishTitle()}
          >
            <Show when={!barBusy()} fallback={<IconSpinner />}>
              <Show when={action() !== 'commit'} fallback={<IconCheck />}>
                <IconArrowUp />
              </Show>
            </Show>
            <span>{COMMIT_LABELS[action()]}</span>
          </button>
        </div>
      </Show>
    </div>
  );
}
