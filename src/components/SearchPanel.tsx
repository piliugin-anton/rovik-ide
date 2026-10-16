// SearchPanel.tsx
//
// Text search across the project for the Files tab, on ripgrep's engine
// (src-tauri/src/code_search.rs). Results are grouped by file; clicking a
// line opens the file with the match selected.
import { createEffect, createMemo, createSignal, For, on, onCleanup, Show } from 'solid-js';
import type { Accessor } from 'solid-js';
import type { ToolSearchLine, ToolSearchResult } from '../types';
import { searchInFiles } from '../lib/commands';
import HighlightedText from './HighlightedText';

const DEBOUNCE_MS = 250;
const MAX_RESULTS = 2000;

interface FileGroup {
  path: string;
  name: string;
  dir: string;
  lines: ToolSearchLine[];
}

export interface SearchMatchTarget {
  /** Path relative to the project root, as returned by the search. */
  relativePath: string;
  line: number;
  column: number;
  length: number;
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
  rootPath: Accessor<string | null>;
  onOpenMatch: (target: SearchMatchTarget) => void;
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

  const [result, setResult] = createSignal<ToolSearchResult | null>(null);
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
        props.rootPath(),
        refreshTick(),
      ],
      () => {
        window.clearTimeout(timer);
        const current = ++run;
        const root = props.rootPath();
        const q = query();
        if (!root || !q.trim()) {
          setResult(null);
          setError(null);
          setSearching(false);
          return;
        }
        setSearching(true);
        timer = window.setTimeout(async () => {
          try {
            const r = await searchInFiles(root, q, {
              regex: regex(),
              caseSensitive: caseSensitive(),
              wholeWord: wholeWord(),
              include: include(),
              exclude: exclude(),
              maxResults: MAX_RESULTS,
            });
            if (current !== run) return;
            setResult(r);
            setError(null);
            setCollapsed(new Set<string>());
          } catch (err) {
            if (current !== run) return;
            setResult(null);
            setError(String(err));
          } finally {
            if (current === run) setSearching(false);
          }
        }, DEBOUNCE_MS);
      }
    )
  );

  const groups = createMemo<FileGroup[]>(() => {
    const out: FileGroup[] = [];
    for (const line of result()?.lines ?? []) {
      if (!line.isMatch) continue;
      let group = out[out.length - 1];
      if (!group || group.path !== line.path) {
        group = { path: line.path, ...splitPath(line.path), lines: [] };
        out.push(group);
      }
      group.lines.push(line);
    }
    return out;
  });

  function toggleGroup(path: string) {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }

  function open(line: ToolSearchLine) {
    const first = line.ranges?.[0];
    props.onOpenMatch({
      relativePath: line.path,
      line: line.line,
      column: line.column ?? 1,
      length: first ? first[1] - first[0] : 0,
    });
  }

  const summary = () => {
    const r = result();
    if (!r) return '';
    if (r.matchCount === 0) return 'No results';
    // The whole project's counts; "+" when counting stopped before the end.
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
          <span>{searching() && !result() ? 'Searching…' : summary()}</span>
          <Show when={result()}>
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
          <For each={groups()}>
            {(group) => (
              <div class="search-file">
                <button
                  type="button"
                  class="search-file-header"
                  aria-expanded={!collapsed().has(group.path)}
                  onClick={() => toggleGroup(group.path)}
                  title={group.path}
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
                <Show when={!collapsed().has(group.path)}>
                  <For each={group.lines}>
                    {(line) => (
                      <button
                        type="button"
                        class="search-line"
                        onClick={() => open(line)}
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
        </div>
      </Show>
    </div>
  );
}
