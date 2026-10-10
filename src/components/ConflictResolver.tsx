// ConflictResolver.tsx
//
// Resolves the conflicts a merge or rebase left: the files git couldn't
// merge on the left, and for the one picked three columns side by side —
// your version, the result, the incoming version. Each conflict is settled
// by taking one side, both (in the order taken), or by editing the result
// by hand; any choice can be changed again, also after the file is saved,
// until the merge is committed or the rebase continued. The choices and
// the file they make are in src/lib/conflictMerge.ts; reading and writing
// the files, and continuing or aborting, in src-tauri/src/git_sync.rs.
//
// An isolated chat's conflicts with the project (IsolationBar.tsx) use the
// same dialog through `source`: the project's version on the left, the
// chat's on the right, and Land to finish (src-tauri/src/overlay/chats.rs).

import {
  createEffect,
  createMemo,
  createSignal,
  For,
  on,
  onCleanup,
  onMount,
  Show,
} from 'solid-js';
import { Portal } from 'solid-js/web';
import { invoke } from '@tauri-apps/api/core';
import type { GitConflictFile, GitMergeSegment, GitOperation } from '../types';
import {
  allTo,
  composeResult,
  conflictsOf,
  hunkText,
  isResolved,
  toggleSide,
  type HunkChoice,
  type Side,
} from '../lib/conflictMerge';
import { confirmDialog } from './ConfirmDialog';

/** Conflicts that aren't git's: where they're read and settled, and words. */
export interface ConflictSource {
  load(file: string): Promise<GitConflictFile>;
  /** `content` null: delete the file; `side`: take that whole version. */
  resolve(file: string, content: string | null, side?: Side): Promise<void>;
  title: string;
  /** No conflicts left: what finishing does. */
  doneHint: string;
  /** The finish button. */
  finishLabel: string;
  /** What the two sides are called, capitalized (git's: Yours, Incoming). */
  sideNames: { ours: string; theirs: string };
}

interface ConflictResolverProps {
  projectPath: string;
  source?: ConflictSource;
  /** Nothing can be saved or finished now (a chat's run is going). */
  disabled?: boolean;
  operation: GitOperation;
  /** The file to show first. */
  initialFile?: string;
  /** A file was written (its open tab should reload). */
  onFileWritten: (file: string) => void;
  /** Commit the merge / continue the rebase (message: merge only). */
  onContinue: (message?: string) => Promise<void>;
  onAbort: () => Promise<void>;
  onClose: () => void;
}

// Choices outlive the dialog (closed and opened again, or a file saved and
// picked again), for as long as the same operation step is in progress.
const savedChoices = new Map<string, (HunkChoice | undefined)[]>();
// Files saved during that step: listed still, so they can be changed again.
const savedFiles = new Map<string, Set<string>>();

/** Forgets the saved choices and files of every step under this project path prefix. */
export function forgetConflictChoices(prefix: string) {
  for (const k of [...savedChoices.keys()]) if (k.startsWith(prefix)) savedChoices.delete(k);
  for (const k of [...savedFiles.keys()]) if (k.startsWith(prefix)) savedFiles.delete(k);
}

const opKey = (projectPath: string, op: GitOperation) =>
  `${projectPath}\0${op.kind}\0${op.branch ?? ''}\0${op.incoming ?? ''}\0${op.step ?? ''}`;

/** A text's lines, without the empty one after a final newline. */
function linesOf(text: string): string[] {
  if (!text) return [];
  const lines = text.split('\n');
  if (lines[lines.length - 1] === '') lines.pop();
  return lines.map((l) => l.replace(/\r$/, ''));
}

/** Shared lines further than this from a conflict are folded away. */
const CONTEXT = 3;

function CodeLines(props: { text: string; start: number; empty?: string }) {
  const lines = () => linesOf(props.text);
  return (
    <Show
      when={lines().length > 0}
      fallback={<div class="cr-empty-lines">{props.empty ?? 'No lines'}</div>}
    >
      <For each={lines()}>
        {(line, i) => (
          <div class="cr-line">
            <span class="cr-ln">{props.start + i()}</span>
            <span class="cr-code">{line || ' '}</span>
          </div>
        )}
      </For>
    </Show>
  );
}

/** Shared lines: all three columns alike, a long run folded in the middle. */
function CommonRow(props: {
  text: string;
  starts: [number, number, number];
  first: boolean;
  last: boolean;
}) {
  const [open, setOpen] = createSignal(false);
  const lines = () => linesOf(props.text);
  const head = () => (props.first ? 0 : CONTEXT);
  const tail = () => (props.last ? 0 : CONTEXT);
  const folded = () => !open() && lines().length > head() + tail() + 2;
  const piece = (from: number, to: number) =>
    lines()
      .slice(from, to)
      .map((l) => `${l}\n`)
      .join('');
  const column = (col: 0 | 1 | 2) => (
    <div class="cr-cell cr-common">
      <Show when={folded()} fallback={<CodeLines text={props.text} start={props.starts[col]} />}>
        <CodeLines text={piece(0, head())} start={props.starts[col]} empty="" />
        <button type="button" class="cr-fold" onClick={() => setOpen(true)}>
          ⋯ {lines().length - head() - tail()} unchanged lines
        </button>
        <CodeLines
          text={piece(lines().length - tail(), lines().length)}
          start={props.starts[col] + lines().length - tail()}
          empty=""
        />
      </Show>
    </div>
  );
  return (
    <>
      {column(0)}
      {column(1)}
      {column(2)}
    </>
  );
}

export function ConflictResolver(props: ConflictResolverProps) {
  const key = () => opKey(props.projectPath, props.operation);
  const doneFiles = () => {
    let set = savedFiles.get(key());
    if (!set) savedFiles.set(key(), (set = new Set()));
    return set;
  };

  const [version, setVersion] = createSignal(0); // bumps when doneFiles changes
  const files = createMemo(() => {
    version();
    const all = new Set([...props.operation.conflicts, ...doneFiles()]);
    return [...all].sort();
  });
  const isPending = (file: string) => props.operation.conflicts.includes(file);

  const [selected, setSelected] = createSignal<string | null>(
    props.initialFile ?? props.operation.conflicts[0] ?? null
  );
  const [data, setData] = createSignal<GitConflictFile | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [choices, setChoices] = createSignal<(HunkChoice | undefined)[]>([]);
  const [editing, setEditing] = createSignal<number | null>(null);
  const [saving, setSaving] = createSignal(false);
  const [message, setMessage] = createSignal('');
  const [finishing, setFinishing] = createSignal(false);

  const choiceKey = (file: string) => `${key()}\0${file}`;

  // A file vanished from the list (the step changed): pick another.
  createEffect(() => {
    const list = files();
    const current = selected();
    if (!current || !list.includes(current)) setSelected(list.find(isPending) ?? list[0] ?? null);
  });

  createEffect(
    on(selected, async (file) => {
      setData(null);
      setError(null);
      setEditing(null);
      if (!file) return;
      setLoading(true);
      try {
        const d = props.source
          ? await props.source.load(file)
          : await invoke<GitConflictFile>('git_conflict_file', {
              path: props.projectPath,
              file,
            });
        if (selected() !== file) return;
        setData(d);
        setChoices(savedChoices.get(choiceKey(file)) ?? []);
      } catch (e) {
        if (selected() === file) setError(String(e));
      } finally {
        if (selected() === file) setLoading(false);
      }
    })
  );

  const choose = (index: number, next: HunkChoice | undefined) => {
    const file = selected();
    if (!file) return;
    const list = [...choices()];
    list[index] = next;
    setChoices(list);
    savedChoices.set(choiceKey(file), list);
  };
  const setAll = (list: (HunkChoice | undefined)[]) => {
    const file = selected();
    if (!file) return;
    setChoices(list);
    setEditing(null);
    savedChoices.set(choiceKey(file), list);
  };

  const conflicts = createMemo(() => conflictsOf(data()?.segments ?? []));
  const composed = createMemo(() => {
    const d = data();
    return d ? composeResult(d, choices()) : null;
  });

  /** Each segment, with where it starts in the side columns and its
   *  conflict index. Only the file changes it, so rows (and a textarea being
   *  typed in) aren't rebuilt when a choice changes. */
  const rows = createMemo(() => {
    const segs = data()?.segments ?? [];
    let o = 1;
    let t = 1;
    let c = 0;
    return segs.map((seg, i) => {
      const starts = { ours: o, theirs: t };
      if (seg.kind === 'common') {
        const n = linesOf(seg.text).length;
        o += n;
        t += n;
        return { seg, i, starts, index: -1, first: i === 0, last: i === segs.length - 1 };
      }
      o += linesOf(seg.ours).length;
      t += linesOf(seg.theirs).length;
      return { seg, i, starts, index: c++, first: false, last: false };
    });
  });
  /** Where each segment starts in the result, which the choices decide. */
  const resultStarts = createMemo(() => {
    let r = 1;
    return rows().map((row) => {
      const start = r;
      const text =
        row.seg.kind === 'common' ? row.seg.text : hunkText(row.seg, choices()[row.index]);
      r += linesOf(text).length;
      return start;
    });
  });

  let gridScroll: HTMLDivElement | undefined;
  const scrollToConflict = (dir: 1 | -1) => {
    if (!gridScroll) return;
    const marks = [...gridScroll.querySelectorAll<HTMLElement>('.cr-conflict-anchor')];
    const top = gridScroll.getBoundingClientRect().top;
    const at = marks.map((m) => m.getBoundingClientRect().top - top);
    let target: HTMLElement | undefined;
    if (dir === 1) target = marks.find((_, i) => at[i] > 8);
    else for (let i = marks.length - 1; i >= 0 && !target; i--) if (at[i] < -8) target = marks[i];
    target?.scrollIntoView({ block: 'start', behavior: 'smooth' });
  };

  const save = async () => {
    const d = data();
    const file = selected();
    const result = composed();
    if (!d || !file || !result || result.unresolved > 0) return;
    setSaving(true);
    setError(null);
    try {
      const content = result.deleted ? null : result.text;
      if (props.source) {
        await props.source.resolve(file, content);
        // Settled for good: the file is now merged against the project, so
        // the old choices no longer fit it.
        savedChoices.delete(choiceKey(file));
      } else await invoke('git_conflict_resolve', { path: props.projectPath, file, content });
      doneFiles().add(file);
      setVersion((v) => v + 1);
      props.onFileWritten(file);
      // The next file still to resolve, not one done earlier in this session.
      const next = props.operation.conflicts.find((f) => f !== file && !doneFiles().has(f));
      if (next) setSelected(next);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const takeWhole = async (side: Side) => {
    const file = selected();
    if (!file) return;
    setSaving(true);
    setError(null);
    try {
      if (props.source) await props.source.resolve(file, null, side);
      else await invoke('git_conflict_resolve', { path: props.projectPath, file, side });
      // Every conflict to that side, so coming back shows the file resolved;
      // a file with no segments (binary) keeps the one choice its card shows.
      const segments = data()?.segments ?? [];
      const all = segments.length ? allTo(segments, side) : [{ sides: [side] }];
      savedChoices.set(choiceKey(file), all);
      setChoices(all);
      doneFiles().add(file);
      setVersion((v) => v + 1);
      props.onFileWritten(file);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const sideName = (side: Side) =>
    props.source?.sideNames[side] ?? (side === 'ours' ? 'Yours' : 'Incoming');
  const lower = (side: Side) => sideName(side).toLowerCase();

  const verb = () => (props.operation.kind === 'rebase' ? 'rebase' : props.operation.kind);
  const title = () => {
    if (props.source) return props.source.title;
    const op = props.operation;
    if (op.kind === 'rebase') {
      const step = op.step && op.total ? ` (${op.step} of ${op.total})` : '';
      return `Rebasing ${op.branch ?? 'branch'} onto ${op.incoming ?? 'upstream'}${step}`;
    }
    if (op.kind === 'merge')
      return `Merging ${op.incoming ?? 'incoming changes'} into ${op.branch ?? 'HEAD'}`;
    return `${op.kind[0].toUpperCase()}${op.kind.slice(1)} in progress`;
  };

  const abort = async () => {
    const ok = await confirmDialog({
      title: `Abort the ${verb()}?`,
      message:
        props.operation.kind === 'rebase'
          ? 'Your branch goes back to where it was before the rebase. Resolutions made so far are lost.'
          : 'Your branch goes back to where it was before the merge. Resolutions made so far are lost.',
      confirmLabel: `Abort ${verb()}`,
      danger: true,
    });
    if (!ok) return;
    setFinishing(true);
    try {
      await props.onAbort();
    } finally {
      setFinishing(false);
    }
  };

  const finish = async () => {
    setFinishing(true);
    try {
      await props.onContinue(
        props.operation.kind === 'merge' ? message().trim() || undefined : undefined
      );
    } finally {
      setFinishing(false);
    }
  };

  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && editing() === null && !document.querySelector('.confirm-dialog')) {
        e.preventDefault();
        props.onClose();
      }
    };
    window.addEventListener('keydown', onKey);
    onCleanup(() => window.removeEventListener('keydown', onKey));
  });

  const sideCell = (
    seg: Extract<GitMergeSegment, { kind: 'conflict' }>,
    index: number,
    side: Side,
    start: number
  ) => {
    const choice = () => choices()[index];
    const taken = () => choice()?.custom === undefined && (choice()?.sides.includes(side) ?? false);
    const order = () =>
      choice()?.sides.length === 2 && taken() ? choice()!.sides.indexOf(side) + 1 : null;
    const deleted = () => data()?.[side] === null;
    const label = () => sideName(side);
    return (
      <div
        class={`cr-cell cr-side cr-${side}`}
        classList={{ 'cr-taken': taken(), 'cr-dropped': isResolved(choice()) && !taken() }}
      >
        <div class="cr-hunk-bar">
          <button
            type="button"
            class="cr-accept"
            classList={{ on: taken() }}
            aria-pressed={taken()}
            title={
              taken()
                ? `Drop ${label().toLowerCase()} from the result`
                : isResolved(choice()) && choice()?.custom === undefined
                  ? `Add ${label().toLowerCase()} to the result, after what's there`
                  : `Use ${label().toLowerCase()}`
            }
            onClick={() => {
              setEditing(null);
              choose(index, toggleSide(choice(), side));
            }}
          >
            <span class="cr-check" aria-hidden="true">
              {taken() ? '✓' : ''}
            </span>
            {taken() ? 'Accepted' : `Accept ${label().toLowerCase()}`}
            <Show when={order()}>
              <span class="cr-order" title="Position in the result">
                {order()}
              </span>
            </Show>
          </button>
          <button
            type="button"
            class="cr-only"
            title={`Use only ${label().toLowerCase()}`}
            onClick={() => {
              setEditing(null);
              choose(index, { sides: [side] });
            }}
          >
            Only this
          </button>
        </div>
        <CodeLines
          text={seg[side]}
          start={start}
          empty={deleted() ? 'Deleted in this version' : 'No lines here'}
        />
      </div>
    );
  };

  const resultCell = (
    seg: Extract<GitMergeSegment, { kind: 'conflict' }>,
    index: number,
    start: () => number
  ) => {
    const choice = () => choices()[index];
    const state = () => {
      const c = choice();
      if (!isResolved(c)) return 'Unresolved';
      if (c!.custom !== undefined) return 'Edited';
      return c!.sides.map(sideName).join(' + ');
    };
    return (
      <div class="cr-cell cr-result" classList={{ 'cr-unresolved': !isResolved(choice()) }}>
        <div class="cr-hunk-bar cr-conflict-anchor">
          <span class="cr-state">{state()}</span>
          <span class="cr-spacer" />
          <button
            type="button"
            class="cr-mini"
            onClick={() => {
              if (editing() === index) return setEditing(null);
              if (choice()?.custom === undefined)
                choose(index, { sides: choice()?.sides ?? [], custom: hunkText(seg, choice()) });
              setEditing(index);
            }}
          >
            {editing() === index ? 'Done' : 'Edit'}
          </button>
          <Show when={isResolved(choice())}>
            <button
              type="button"
              class="cr-mini"
              title="Undo the choice for this conflict"
              onClick={() => {
                setEditing(null);
                choose(index, undefined);
              }}
            >
              Reset
            </button>
          </Show>
        </div>
        <Show
          when={editing() === index}
          fallback={
            <Show
              when={isResolved(choice())}
              fallback={
                <div class="cr-empty-lines">
                  Accept {lower('ours')}, {lower('theirs')}, or both — or edit
                </div>
              }
            >
              <CodeLines
                text={hunkText(seg, choice())}
                start={start()}
                empty="Removed (no lines)"
              />
            </Show>
          }
        >
          <textarea
            class="cr-edit"
            spellcheck={false}
            rows={Math.max(3, linesOf(hunkText(seg, choice())).length + 1)}
            value={hunkText(seg, choice())}
            ref={(el) => queueMicrotask(() => el.focus())}
            onInput={(e) =>
              choose(index, { sides: choice()?.sides ?? [], custom: e.currentTarget.value })
            }
            onKeyDown={(e) => {
              if (e.key === 'Escape') {
                e.stopPropagation();
                setEditing(null);
              }
            }}
          />
        </Show>
      </div>
    );
  };

  const resolvedCount = () => conflicts().length - (composed()?.unresolved ?? 0);

  return (
    <Portal>
      <div class="modal-overlay">
        <div
          class="modal conflict-resolver"
          role="dialog"
          aria-modal="true"
          aria-label="Resolve conflicts"
        >
          <div class="modal-header cr-header">
            <div class="cr-title">
              <span>{title()}</span>
              <Show when={props.operation.kind === 'rebase' && props.operation.commit}>
                <span class="cr-subtitle">Applying: {props.operation.commit}</span>
              </Show>
            </div>
            <button
              type="button"
              class="cr-close"
              title="Close (Esc)"
              onClick={() => props.onClose()}
            >
              ✕
            </button>
          </div>

          <div class="cr-body">
            <div class="cr-files">
              <div class="cr-files-label">
                {props.operation.conflicts.length > 0
                  ? `${props.operation.conflicts.length} conflicted file${props.operation.conflicts.length === 1 ? '' : 's'}`
                  : 'All conflicts resolved'}
              </div>
              <For each={files()}>
                {(file) => (
                  <button
                    type="button"
                    class="cr-file"
                    classList={{ active: selected() === file, done: !isPending(file) }}
                    title={file}
                    onClick={() => setSelected(file)}
                  >
                    <span class="cr-file-dot" aria-hidden="true">
                      {isPending(file) ? '!' : '✓'}
                    </span>
                    <span class="cr-file-name">{file.split('/').pop()}</span>
                    <span class="cr-file-dir">
                      {file.includes('/') ? file.slice(0, file.lastIndexOf('/')) : ''}
                    </span>
                  </button>
                )}
              </For>
            </div>

            <div class="cr-main">
              <Show when={error()}>
                <div class="git-inline-error cr-error">{error()}</div>
              </Show>
              <Show when={loading()}>
                <div class="empty-state">Loading…</div>
              </Show>
              <Show when={!selected() && !loading()}>
                <div class="empty-state">
                  {props.source
                    ? `No conflicts left. ${props.source.doneHint}`
                    : props.operation.canContinue
                      ? `No conflicts left. ${props.operation.kind === 'merge' ? 'Commit the merge' : 'Continue the rebase'} to finish.`
                      : 'No conflicts left.'}
                </div>
              </Show>

              <Show when={data()}>
                {(d) => (
                  <>
                    <div class="cr-toolbar">
                      <span class="cr-path" title={d().path}>
                        {d().path}
                      </span>
                      <Show when={!d().binary}>
                        <span
                          class="cr-progress"
                          classList={{ complete: composed()?.unresolved === 0 }}
                        >
                          {resolvedCount()} of {conflicts().length} resolved
                        </span>
                        <button
                          type="button"
                          class="cr-tool"
                          title="Previous conflict"
                          onClick={() => scrollToConflict(-1)}
                        >
                          ↑
                        </button>
                        <button
                          type="button"
                          class="cr-tool"
                          title="Next conflict"
                          onClick={() => scrollToConflict(1)}
                        >
                          ↓
                        </button>
                        <span class="cr-spacer" />
                        <button
                          type="button"
                          class="cr-tool"
                          onClick={() => setAll(allTo(d().segments, 'ours'))}
                        >
                          Accept all {lower('ours')}
                        </button>
                        <button
                          type="button"
                          class="cr-tool"
                          onClick={() => setAll(allTo(d().segments, 'theirs'))}
                        >
                          Accept all {lower('theirs')}
                        </button>
                        <button type="button" class="cr-tool" onClick={() => setAll([])}>
                          Reset
                        </button>
                        <button
                          type="button"
                          class="primary cr-save"
                          disabled={
                            saving() || !!props.disabled || (composed()?.unresolved ?? 1) > 0
                          }
                          title={
                            (composed()?.unresolved ?? 0) > 0
                              ? 'Resolve every conflict of this file first'
                              : composed()?.deleted
                                ? 'Delete the file and mark it resolved'
                                : 'Write the result and mark the file resolved'
                          }
                          onClick={() => void save()}
                        >
                          {composed()?.deleted
                            ? 'Delete file & mark resolved'
                            : d().resolved || !isPending(d().path)
                              ? 'Save again'
                              : 'Mark resolved'}
                        </button>
                      </Show>
                    </div>

                    <Show when={!isPending(d().path)}>
                      <div class="cr-note">
                        {props.source
                          ? 'Resolved: this is the version that lands.'
                          : 'Resolved and staged. Choices can still be changed — save again to replace the resolution.'}
                      </div>
                    </Show>

                    <Show
                      when={!d().binary}
                      fallback={
                        <div class="cr-binary">
                          <p>This file isn't text: take one version as a whole.</p>
                          <div class="cr-binary-choices">
                            <For each={['ours', 'theirs'] as Side[]}>
                              {(side) => (
                                <button
                                  type="button"
                                  class="cr-binary-choice"
                                  classList={{ on: choices()[0]?.sides[0] === side }}
                                  disabled={saving() || !!props.disabled}
                                  onClick={() => void takeWhole(side)}
                                >
                                  <strong>
                                    {side === 'ours' ? d().oursLabel : d().theirsLabel}
                                  </strong>
                                  <span>
                                    {d()[side] === null
                                      ? 'Deleted in this version'
                                      : 'Use this version'}
                                  </span>
                                </button>
                              )}
                            </For>
                          </div>
                        </div>
                      }
                    >
                      <div class="cr-columns-head">
                        <div class="cr-col-title cr-ours" title={d().oursLabel}>
                          {d().oursLabel}
                        </div>
                        <div class="cr-col-title cr-result-title">
                          Result
                          <Show when={composed()?.deleted}>
                            <span class="cr-deleted-tag">file will be deleted</span>
                          </Show>
                        </div>
                        <div class="cr-col-title cr-theirs" title={d().theirsLabel}>
                          {d().theirsLabel}
                        </div>
                      </div>
                      <div class="cr-grid-scroll" ref={gridScroll}>
                        <div class="cr-grid">
                          <For each={rows()}>
                            {(row) =>
                              row.seg.kind === 'common' ? (
                                <CommonRow
                                  text={row.seg.text}
                                  starts={[
                                    row.starts.ours,
                                    resultStarts()[row.i],
                                    row.starts.theirs,
                                  ]}
                                  first={row.first}
                                  last={row.last}
                                />
                              ) : (
                                <>
                                  {sideCell(row.seg, row.index, 'ours', row.starts.ours)}
                                  {resultCell(row.seg, row.index, () => resultStarts()[row.i])}
                                  {sideCell(row.seg, row.index, 'theirs', row.starts.theirs)}
                                </>
                              )
                            }
                          </For>
                        </div>
                      </div>
                    </Show>
                  </>
                )}
              </Show>
            </div>
          </div>

          <div class="cr-footer">
            <Show when={!props.source}>
              <button
                type="button"
                class="danger-outline"
                disabled={finishing()}
                onClick={() => void abort()}
              >
                Abort {verb()}
              </button>
            </Show>
            <span class="cr-spacer" />
            <Show when={props.operation.kind === 'merge' && !props.source}>
              <input
                class="tree-row-input cr-message"
                placeholder="Merge commit message (optional)"
                value={message()}
                onInput={(e) => setMessage(e.currentTarget.value)}
              />
            </Show>
            <Show when={props.operation.conflicts.length > 0}>
              <span class="cr-footer-note">
                {props.operation.conflicts.length} file
                {props.operation.conflicts.length === 1 ? '' : 's'} still conflicted
              </span>
            </Show>
            <Show when={props.operation.canContinue}>
              <button
                type="button"
                class="primary"
                disabled={finishing() || !!props.disabled || props.operation.conflicts.length > 0}
                onClick={() => void finish()}
              >
                {props.source?.finishLabel ??
                  (props.operation.kind === 'merge' ? 'Commit merge' : 'Continue rebase')}
              </button>
            </Show>
          </div>
        </div>
      </div>
    </Portal>
  );
}
