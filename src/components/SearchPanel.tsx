// SearchPanel.tsx
//
// Text search across the window's folders for the Files tab, on ripgrep's
// engine (src-tauri/src/code_search.rs). Every folder is searched at once and
// the results are grouped by file, folder by folder (C02); clicking a line
// opens the file with the match selected.
import { createEffect, createMemo, createSignal, For, on, onCleanup, Show } from 'solid-js';
import type { Accessor } from 'solid-js';
import type { ToolSearchLine, ToolSearchResult } from '../types';
import type { OpenFolder } from '../lib/openFolders';
import { toAbsolutePath } from '../lib/fileTools';
import { searchInFiles } from '../lib/commands';
import { openInEditor } from '../lib/openInEditor';
import HighlightedText from './HighlightedText';

const DEBOUNCE_MS = 250;
const MAX_RESULTS = 2000;

interface FileGroup {
  /** The folder the file is in: a workspace has several, and each has its own. */
  folder: OpenFolder;
  /** The group's collapsed state is kept under this: two folders have the same relative paths. */
  key: string;
  /** The file's path, relative to its folder (what the search answered). */
  path: string;
  name: string;
  dir: string;
  lines: ToolSearchLine[];
}

/** One folder's answer to the search. */
export interface FolderSearch {
  folder: OpenFolder;
  result: ToolSearchResult;
}

/** A folder whose search failed (it went away, the backend refused): the others still show. */
export interface FolderFailure {
  folder: OpenFolder;
  error: string;
}

/** Every folder's matches, kept as one search. */
export interface MergedSearch {
  /** The lines kept, folder by folder, in the order searched. */
  perFolder: { folder: OpenFolder; lines: ToolSearchLine[] }[];
  /** Matches and files kept in `perFolder`. */
  matchCount: number;
  fileCount: number;
  /** A folder had more than it could give, or the limit cut lines. */
  truncated: boolean;
  /** Matches and files in the folders' whole search, shown or not. */
  totalMatches: number;
  totalFiles: number;
  /** false when a folder stopped counting early: the totals are at least this many. */
  totalsComplete: boolean;
}

/**
 * One search over every folder (C02): the matches in folder order, up to one
 * limit for the whole search, with the folders' totals added up. A folder's
 * own truncation carries over, so the summary says more exist.
 */
export function mergeResults(perFolder: readonly FolderSearch[], limit: number): MergedSearch {
  const kept: MergedSearch['perFolder'] = [];
  const files = new Set<string>();
  let lines = 0;
  let matchCount = 0;
  let truncated = false;
  let totalMatches = 0;
  let totalFiles = 0;
  let totalsComplete = true;
  for (const { folder, result } of perFolder) {
    const room = Math.max(0, limit - lines);
    const inside = result.lines.slice(0, room);
    // Cut here, or cut by the folder's own search: either way more exist.
    if (inside.length < result.lines.length || result.truncated) truncated = true;
    lines += inside.length;
    for (const line of inside) {
      if (!line.isMatch) continue;
      matchCount++;
      files.add(`${folder.path}\n${line.path}`);
    }
    kept.push({ folder, lines: inside });
    totalMatches += result.totalMatches;
    totalFiles += result.totalFiles;
    totalsComplete &&= result.totalsComplete;
  }
  return {
    perFolder: kept,
    matchCount,
    fileCount: files.size,
    truncated,
    totalMatches,
    totalFiles,
    totalsComplete,
  };
}

/** The matches of one folder's lines, grouped by file, in path order. */
function groupsIn(folder: OpenFolder, lines: readonly ToolSearchLine[]): FileGroup[] {
  const out: FileGroup[] = [];
  for (const line of lines) {
    if (!line.isMatch) continue;
    let group = out[out.length - 1];
    if (!group || group.path !== line.path) {
      group = {
        folder,
        key: `${folder.path}\n${line.path}`,
        path: line.path,
        ...splitPath(line.path),
        lines: [],
      };
      out.push(group);
    }
    group.lines.push(line);
  }
  return out;
}

function splitPath(path: string): { name: string; dir: string } {
  const normalized = path.replace(/\\/g, '/');
  const slash = normalized.lastIndexOf('/');
  return slash < 0
    ? { name: normalized, dir: '' }
    : { name: normalized.slice(slash + 1), dir: normalized.slice(0, slash) };
}

/**
 * The search as it was left: the panel goes while another sidebar tab is
 * shown, and coming back to Files showed an empty search.
 */
const kept = {
  query: '',
  caseSensitive: false,
  wholeWord: false,
  regex: false,
  showGlobs: false,
  include: '',
  exclude: '',
};

export default function SearchPanel(props: {
  /** The folders to search, every one of them at once (a workspace has several). */
  folders: Accessor<readonly OpenFolder[]>;
  /** True while a query is entered (the Files tab then hides the tree). */
  onActiveChange?: (active: boolean) => void;
}) {
  const [query, setQuery] = createSignal(kept.query);
  const [caseSensitive, setCaseSensitive] = createSignal(kept.caseSensitive);
  const [wholeWord, setWholeWord] = createSignal(kept.wholeWord);
  const [regex, setRegex] = createSignal(kept.regex);
  const [showGlobs, setShowGlobs] = createSignal(kept.showGlobs);
  const [include, setInclude] = createSignal(kept.include);
  const [exclude, setExclude] = createSignal(kept.exclude);
  createEffect(() =>
    Object.assign(kept, {
      query: query(),
      caseSensitive: caseSensitive(),
      wholeWord: wholeWord(),
      regex: regex(),
      showGlobs: showGlobs(),
      include: include(),
      exclude: exclude(),
    })
  );

  const [results, setResults] = createSignal<FolderSearch[] | null>(null);
  /** The folders that couldn't be searched, with what the backend said. */
  const [failed, setFailed] = createSignal<FolderFailure[]>([]);
  const [error, setError] = createSignal<string | null>(null);
  const [searching, setSearching] = createSignal(false);
  const [collapsed, setCollapsed] = createSignal<Set<string>>(new Set());
  const [refreshTick, setRefreshTick] = createSignal(0);

  const active = () => query().trim().length > 0;
  createEffect(() => props.onActiveChange?.(active()));

  /*
   * Debounced search; results of an older run that arrive late are dropped.
   */
  let timer: number | undefined;
  let run = 0;
  onCleanup(() => window.clearTimeout(timer));

  createEffect(
    on(
      () => [
        query(),
        caseSensitive(),
        wholeWord(),
        regex(),
        include(),
        exclude(),
        props
          .folders()
          .map((folder) => folder.path)
          .join('\n'),
        refreshTick(),
      ],
      () => {
        window.clearTimeout(timer);
        const current = ++run;
        const folders = props.folders();
        const q = query();
        if (folders.length === 0 || !q.trim()) {
          setResults(null);
          setFailed([]);
          setError(null);
          setSearching(false);
          return;
        }
        setSearching(true);
        timer = window.setTimeout(async () => {
          // Every folder at once: a workspace's search is one search (C02).
          // A folder that fails (its drive went, the backend refused) leaves
          // the others' results shown, with its own note beside its header.
          const options = {
            regex: regex(),
            caseSensitive: caseSensitive(),
            wholeWord: wholeWord(),
            include: include(),
            exclude: exclude(),
            maxResults: MAX_RESULTS,
          };
          const settled = await Promise.allSettled(
            folders.map(async (folder) => ({
              folder,
              result: await searchInFiles(folder.path, q, options),
            }))
          );
          if (current !== run) return;
          const found: FolderSearch[] = [];
          const failures: FolderFailure[] = [];
          settled.forEach((outcome, i) => {
            if (outcome.status === 'fulfilled') found.push(outcome.value);
            else failures.push({ folder: folders[i], error: String(outcome.reason) });
          });
          // Nothing answered: the panel says why, as with one folder. Some
          // did: their results are the search, and each failure its folder's.
          const nothing = found.length === 0 && failures.length > 0;
          setResults(nothing ? null : found);
          setFailed(nothing ? [] : failures);
          setError(nothing ? failures.map((f) => `${f.folder.path}: ${f.error}`).join('\n') : null);
          setCollapsed(new Set<string>());
          if (current === run) setSearching(false);
        }, DEBOUNCE_MS);
      }
    )
  );

  const merged = createMemo(() => {
    const found = results();
    return found ? mergeResults(found, MAX_RESULTS) : null;
  });

  /** The groups, folder by folder, in the order the folders are searched; a failed one says why. */
  const sections = createMemo(() => {
    const parts = merged()?.perFolder ?? [];
    const failures = failed();
    return props.folders().map((folder) => {
      const part = parts.find((each) => each.folder.path === folder.path);
      return {
        folder,
        groups: part ? groupsIn(folder, part.lines) : [],
        error: failures.find((each) => each.folder.path === folder.path)?.error ?? null,
      };
    });
  });

  /** A workspace's results say which folder each is in; a project's don't (C02). */
  const several = () => props.folders().length > 1;

  function toggleGroup(key: string) {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }

  /** Opens the match's file, in the folder the match is in, with the match selected. */
  function open(folder: OpenFolder, line: ToolSearchLine) {
    const first = line.ranges?.[0];
    void openInEditor({
      path: toAbsolutePath(line.path, folder.path),
      focus: true,
      highlight: 'selection',
      at: { line: line.line, column: line.column ?? 1, length: first ? first[1] - first[0] : 0 },
    });
  }

  const summary = () => {
    const r = merged();
    if (!r) return '';
    if (r.matchCount === 0) return 'No results';
    // The whole search's counts; "+" when counting stopped before the end.
    const more = r.totalsComplete ? '' : '+';
    const results = `${r.totalMatches.toLocaleString()}${more} ${r.totalMatches === 1 && !more ? 'result' : 'results'}`;
    const files = `${r.totalFiles.toLocaleString()}${more} ${r.totalFiles === 1 && !more ? 'file' : 'files'}`;
    return `${results} in ${files}${r.truncated ? ` (first ${r.matchCount.toLocaleString()} shown)` : ''}`;
  };

  return (
    <div class="search-panel" classList={{ active: active() }}>
      <div class="search-row">
        <div class="search-input-wrap">
          <input
            class="search-input"
            value={query()}
            onInput={(e) => setQuery(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === 'Escape' && query()) {
                e.preventDefault();
                setQuery('');
              } else if (e.key === 'Enter') {
                e.preventDefault();
                setRefreshTick((t) => t + 1);
              }
            }}
            placeholder="Search in files"
            spellcheck={false}
            aria-label="Search text in project files"
          />
          <button
            type="button"
            class="search-toggle"
            aria-pressed={caseSensitive()}
            title="Match case"
            onClick={() => setCaseSensitive((v) => !v)}
          >
            Aa
          </button>
          <button
            type="button"
            class="search-toggle whole-word"
            aria-pressed={wholeWord()}
            title="Match whole word"
            onClick={() => setWholeWord((v) => !v)}
          >
            ab
          </button>
          <button
            type="button"
            class="search-toggle"
            aria-pressed={regex()}
            title="Use regular expression"
            onClick={() => setRegex((v) => !v)}
          >
            .*
          </button>
        </div>
        <button
          type="button"
          class="search-icon-button"
          aria-pressed={showGlobs()}
          title="Files to include or exclude"
          onClick={() => setShowGlobs((v) => !v)}
        >
          ...
        </button>
      </div>

      <Show when={showGlobs()}>
        <input
          class="search-input search-glob"
          value={include()}
          onInput={(e) => setInclude(e.currentTarget.value)}
          placeholder="Files to include, e.g. src/**, *.ts"
          spellcheck={false}
          aria-label="Files to include"
        />
        <input
          class="search-input search-glob"
          value={exclude()}
          onInput={(e) => setExclude(e.currentTarget.value)}
          placeholder="Files to exclude, e.g. *.test.ts, docs/**"
          spellcheck={false}
          aria-label="Files to exclude"
        />
      </Show>

      <Show when={active()}>
        <div class="search-status">
          <span>{searching() && !results() ? 'Searching…' : summary()}</span>
          <Show when={merged()}>
            <button
              type="button"
              class="search-icon-button"
              title="Search again (files may have changed)"
              onClick={() => setRefreshTick((t) => t + 1)}
            >
              ↻
            </button>
          </Show>
        </div>

        <Show when={error()}>
          {(message) => (
            <div class="search-error">
              {message().replace(/^Invalid regex: /, 'Invalid regular expression: ')}
            </div>
          )}
        </Show>

        <div class="search-results" classList={{ stale: searching() }}>
          <For each={sections()}>
            {(section) => (
              <>
                {/* Several folders: each folder's results say which it is. */}
                <Show when={several()}>
                  <div class="search-folder-header" title={section.folder.path}>
                    {section.folder.name}
                  </div>
                </Show>
                {/* A folder that couldn't be searched: the others still show. */}
                <Show when={section.error}>
                  {(why) => (
                    <div class="search-error search-folder-error" title={section.folder.path}>
                      {section.folder.name}: {why()}
                    </div>
                  )}
                </Show>
                <For each={section.groups}>
                  {(group) => (
                    <div class="search-file">
                      <button
                        type="button"
                        class="search-file-header"
                        aria-expanded={!collapsed().has(group.key)}
                        onClick={() => toggleGroup(group.key)}
                        title={`${group.folder.path}/${group.path}`}
                      >
                        <svg
                          class="search-chevron"
                          viewBox="0 0 16 16"
                          width="12"
                          height="12"
                          aria-hidden="true"
                        >
                          <path
                            d="M6 3.5 10.5 8 6 12.5"
                            fill="none"
                            stroke="currentColor"
                            stroke-width="1.6"
                            stroke-linecap="round"
                            stroke-linejoin="round"
                          />
                        </svg>
                        <span class="search-file-name">{group.name}</span>
                        <span class="search-file-dir">{group.dir}</span>
                        <span class="search-file-count">{group.lines.length}</span>
                      </button>
                      <Show when={!collapsed().has(group.key)}>
                        <For each={group.lines}>
                          {(line) => (
                            <button
                              type="button"
                              class="search-line"
                              onClick={() => open(group.folder, line)}
                              title={`Line ${line.line}`}
                            >
                              <span class="search-line-number">{line.line}</span>
                              <span class="search-line-text">
                                <HighlightedText text={line.text} ranges={line.ranges ?? []} />
                              </span>
                            </button>
                          )}
                        </For>
                      </Show>
                    </div>
                  )}
                </For>
              </>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}
