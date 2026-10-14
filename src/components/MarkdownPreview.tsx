// MarkdownPreview.tsx
//
// The rendered view of a Markdown tab, beside its source or instead of it
// (Editor.tsx, lib/markdownView.ts). It shows the tab's text, unsaved edits
// included, rendered as GitHub renders it (lib/markdownPreview.ts):
//   - it scrolls with the editor, both ways, block by block;
//   - links open: web pages in the system browser, project files in a tab,
//     headings in place; local images show, remote ones don't load;
//   - code blocks are colored like the editor (monaco.editor.colorize);
//   - ticking a task box edits the source, as one undoable edit;
//   - in the split view, a double click puts the editor's cursor there.
//
// Only the blocks that changed are replaced on each edit, so images,
// colored code and opened <details> elsewhere stay as they are and the page
// doesn't jump while typing.
import { createEffect, on, onCleanup, onMount, untrack, type Accessor } from 'solid-js';
import * as monaco from 'monaco-editor';
import { invoke } from '@tauri-apps/api/core';
import {
  ANCHOR_PREFIX,
  renderMarkdown,
  resolvePreviewImage,
  resolvePreviewLink,
  sanitizePreviewHtml,
  toggleTaskLine,
} from '../lib/markdownPreview';
import {
  clearHeadingRequest,
  headingRequest,
  requestHeading,
  togglePreview,
  toggleSplit,
} from '../lib/markdownView';
import { readImageFile } from '../lib/commands';
import { editorSettings } from '../lib/editorSettings';
import { applyTheme } from '../lib/monacoThemes';
import { languageForAlias } from '../lib/language';
import { requestOpenFile, resolveProjectFile } from '../lib/editorBridge';
import { toRelativePath } from '../lib/fileTools';
import { notify } from '../lib/notifications';

const isMac = navigator.platform.toUpperCase().includes('MAC');

/** Code blocks longer than this stay uncolored. */
const MAX_COLORIZE = 100_000;
/** Colored code blocks kept for re-renders (typing in one makes a new one each time). */
const MAX_COLORED = 300;

interface MarkdownPreviewProps {
  /** The document: its links and images are relative to it. */
  path: string;
  root: Accessor<string | null>;
  content: Accessor<string>;
  /** The tab's editor: the preview scrolls with it and edits through it. */
  editor: Accessor<monaco.editor.IStandaloneCodeEditor | undefined>;
  /** Shown beside the source (split): the two scroll together. */
  beside: Accessor<boolean>;
  /** The source can't be changed here (a stash or commit preview). */
  readOnly: Accessor<boolean>;
  onSave: () => void;
  ref?: (el: HTMLDivElement) => void;
}

/** A block with its source line, where it is in the preview. */
interface LineBox {
  line: number;
  top: number;
  height: number;
}

/** The last index whose entry passes `test` (entries in order), or -1. */
function lastIndex(boxes: LineBox[], test: (box: LineBox) => boolean): number {
  let lo = 0;
  let hi = boxes.length - 1;
  let found = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (test(boxes[mid])) {
      found = mid;
      lo = mid + 1;
    } else hi = mid - 1;
  }
  return found;
}

/** A node's rendered HTML minus its line numbers: the same block, maybe moved. */
function blockKey(node: Node): string {
  return node instanceof Element
    ? node.outerHTML.replace(/ data-line="\d+"/g, '')
    : (node.textContent ?? '');
}

/** The editor's line at the top of its viewport, 0-based with the part scrolled past. */
function editorTopLine(editor: monaco.editor.IStandaloneCodeEditor): number {
  const model = editor.getModel();
  if (!model) return 0;
  const count = model.getLineCount();
  const scrollTop = editor.getScrollTop();
  let line = editor.getVisibleRanges()[0]?.startLineNumber ?? 1;
  while (line > 1 && editor.getTopForLineNumber(line) > scrollTop) line--;
  while (line < count && editor.getTopForLineNumber(line + 1) <= scrollTop) line++;
  const top = editor.getTopForLineNumber(line);
  const next =
    line < count
      ? editor.getTopForLineNumber(line + 1)
      : top + editor.getOption(monaco.editor.EditorOption.lineHeight);
  return line - 1 + Math.min(1, Math.max(0, (scrollTop - top) / Math.max(1, next - top)));
}

export default function MarkdownPreview(props: MarkdownPreviewProps) {
  let scroller!: HTMLDivElement;
  let body!: HTMLElement;

  /** Each shown block's key (blockKey) as rendered, before images and colors. */
  const keys = new WeakMap<Node, string>();
  /** Blocks with lines and positions; measured when needed, dropped on change. */
  let boxes: LineBox[] | null = null;
  let lineCount = 1;
  let rendered = false;

  // ---------------------------------------------------------------------
  // Rendering
  // ---------------------------------------------------------------------

  /** Replaces only the top-level blocks that changed (same keys at both ends stay). */
  function patch(html: string) {
    const template = document.createElement('template');
    template.innerHTML = html;
    const next = [...template.content.childNodes];
    const nextKeys = next.map(blockKey);
    const prev = [...body.childNodes];
    let start = 0;
    while (start < prev.length && start < next.length && keys.get(prev[start]) === nextKeys[start])
      start++;
    let prevEnd = prev.length;
    let nextEnd = next.length;
    while (
      prevEnd > start &&
      nextEnd > start &&
      keys.get(prev[prevEnd - 1]) === nextKeys[nextEnd - 1]
    ) {
      prevEnd--;
      nextEnd--;
    }
    // Kept blocks may have moved up or down in the source.
    for (let i = 0; i < start; i++) copyLines(prev[i], next[i]);
    for (let i = prevEnd, j = nextEnd; i < prev.length; i++, j++) copyLines(prev[i], next[j]);

    const before = prevEnd < prev.length ? prev[prevEnd] : null;
    for (let i = start; i < prevEnd; i++) prev[i].remove();
    const added = next.slice(start, nextEnd);
    added.forEach((node, i) => keys.set(node, nextKeys[start + i]));
    const fragment = document.createDocumentFragment();
    fragment.append(...added);
    body.insertBefore(fragment, before);
    for (const node of added) if (node instanceof Element) decorate(node);
  }

  /** The new rendering's line numbers onto a kept block (same structure). */
  function copyLines(kept: Node, fresh: Node) {
    if (!(kept instanceof Element) || !(fresh instanceof Element)) return;
    const lined = (el: Element) => [el, ...el.querySelectorAll('[data-line]')];
    const from = lined(fresh);
    const to = lined(kept);
    if (from.length !== to.length) return;
    from.forEach((el, i) => {
      const line = el.getAttribute('data-line');
      if (line !== null && to[i].getAttribute('data-line') !== line)
        to[i].setAttribute('data-line', line);
    });
  }

  function render(text: string) {
    let html: string;
    try {
      html = sanitizePreviewHtml(renderMarkdown(text, { tasks: !props.readOnly() }).html);
    } catch (err) {
      console.error('Markdown preview:', err);
      html = '<p class="md-preview-error">This document couldn’t be rendered.</p>';
    }
    lineCount = text.split('\n').length;
    patch(html);
    boxes = null;
    if (!rendered) {
      rendered = true;
      // Opened where the source was.
      syncPreviewToEditor();
      showRequestedHeading();
    } else if (props.beside()) {
      syncPreviewToEditor();
    }
  }

  // Small documents render on the next tick; big ones once typing pauses.
  let timer: number | undefined;
  createEffect(
    on([props.content, props.readOnly], ([text]) => {
      window.clearTimeout(timer);
      if (!rendered) render(text);
      else timer = window.setTimeout(() => render(text), Math.min(300, text.length / 4000));
    })
  );
  onCleanup(() => window.clearTimeout(timer));

  // ---------------------------------------------------------------------
  // Images and code
  // ---------------------------------------------------------------------

  /** Blob URLs of the images read so far, by path; released with the preview. */
  const images = new Map<string, Promise<string>>();
  onCleanup(() => {
    for (const url of images.values())
      void url.then(
        (u) => URL.revokeObjectURL(u),
        () => {} // a failed read made no URL
      );
  });
  /** Colored code by language and text, so a re-rendered block colors at once. */
  const colored = new Map<string, string>();
  /*
   * Code is colored with the token classes of the editor's theme, once it's
   * loaded (community themes load lazily). Another theme numbers its token
   * colors differently: the code is colored again.
   */
  let themeReady = applyTheme(untrack(() => editorSettings().theme));
  let themeChanges = 0;
  createEffect(
    on(
      () => editorSettings().theme,
      (theme) => {
        themeReady = applyTheme(theme);
        themeChanges++;
        colored.clear();
        for (const code of body.querySelectorAll('pre > code')) colorize(code as HTMLElement);
      },
      { defer: true }
    )
  );

  function decorate(el: Element) {
    const imgs = el.tagName === 'IMG' ? [el] : [...el.querySelectorAll('img[data-src]')];
    for (const img of imgs) showImage(img as HTMLImageElement);
    for (const code of el.querySelectorAll('pre > code')) colorize(code as HTMLElement);
  }

  function showImage(img: HTMLImageElement) {
    const src = img.getAttribute('data-src');
    if (src === null) return;
    const image = resolvePreviewImage(src, props.path, props.root());
    if (image.kind !== 'local') {
      placeholder(
        img,
        image.kind === 'remote'
          ? `Remote image, not loaded: ${image.url}`
          : `Not an image the preview can show: ${src}`
      );
      return;
    }
    let url = images.get(image.path);
    if (!url) {
      const read = readImageFile(image.path).then((bytes) =>
        URL.createObjectURL(new Blob([bytes], { type: image.type }))
      );
      // Not kept when it fails: the file may be added later.
      read.catch(() => images.delete(image.path));
      images.set(image.path, read);
      url = read;
    }
    url.then(
      (u) => {
        img.src = u;
      },
      (err) => {
        if (img.isConnected) placeholder(img, `Image not shown: ${String(err)}`);
      }
    );
  }

  /** An image that isn't shown: its alt text in a box, the reason on hover. */
  function placeholder(img: HTMLImageElement, reason: string) {
    const box = document.createElement('span');
    box.className = 'md-image-placeholder';
    box.textContent = img.alt || 'image';
    box.title = reason;
    img.replaceWith(box);
  }

  function colorize(code: HTMLElement) {
    const text = code.textContent ?? '';
    if (text.length > MAX_COLORIZE) return;
    const lang = /(?:^|\s)language-(\S+)/.exec(code.className)?.[1];
    const key = `${lang ?? ''}\n${text}`;
    const done = colored.get(key);
    if (done !== undefined) {
      code.innerHTML = done;
      return;
    }
    const theme = themeChanges;
    themeReady
      .then(() =>
        monaco.editor.colorize(text.replace(/\n$/, ''), languageForAlias(lang), { tabSize: 4 })
      )
      .then((html) => {
        if (theme !== themeChanges) return; // colored again for the new theme
        // Plain spaces and newlines, so copied code is the code (colorize
        // writes &nbsp; and <br/> as the editor's lines do).
        const plain = html.replace(/&nbsp;/g, ' ').replace(/<br\s*\/?>/g, '\n');
        if (colored.size >= MAX_COLORED) colored.clear();
        colored.set(key, plain);
        if (code.isConnected && code.textContent === text) code.innerHTML = plain;
      })
      .catch(() => {});
  }

  // ---------------------------------------------------------------------
  // Scrolling with the editor
  // ---------------------------------------------------------------------

  /** The blocks' positions, top-down, each starting on a later line than the last. */
  function lineBoxes(): LineBox[] {
    if (boxes) return boxes;
    const out: LineBox[] = [];
    const origin =
      scroller.getBoundingClientRect().top -
      scroller.scrollTop +
      (parseFloat(getComputedStyle(body).paddingTop) || 0);
    for (const el of body.querySelectorAll('[data-line]')) {
      const line = Number(el.getAttribute('data-line'));
      const last = out[out.length - 1];
      if (!Number.isFinite(line) || (last && line <= last.line)) continue;
      const rect = el.getBoundingClientRect();
      // Hidden (in a closed <details>): nowhere to scroll to.
      if (rect.height === 0 && rect.width === 0) continue;
      const top = rect.top - origin;
      if (last && top < last.top) continue;
      out.push({ line, top, height: rect.height });
    }
    boxes = out;
    return out;
  }

  function scrollPreviewToLine(line: number) {
    topLine = line;
    const list = lineBoxes();
    if (list.length === 0) return;
    const i = lastIndex(list, (b) => b.line <= line);
    let top = 0;
    if (i >= 0) {
      const box = list[i];
      const next = list[i + 1];
      top = next
        ? box.top + ((next.top - box.top) * (line - box.line)) / (next.line - box.line)
        : box.top + box.height * Math.min(1, (line - box.line) / Math.max(1, lineCount - box.line));
    }
    setPreviewScroll(top);
  }

  /** The source line at the top of the preview, with the part scrolled past. */
  function previewTopLine(): number {
    const list = lineBoxes();
    if (list.length === 0) return 0;
    const y = scroller.scrollTop;
    const i = lastIndex(list, (b) => b.top <= y);
    if (i < 0) return (list[0].line * y) / Math.max(1, list[0].top);
    const box = list[i];
    const next = list[i + 1];
    if (next)
      return box.line + ((next.line - box.line) * (y - box.top)) / Math.max(1, next.top - box.top);
    return box.line + (lineCount - box.line) * Math.min(1, (y - box.top) / Math.max(1, box.height));
  }

  /*
   * Each side's own scrolling moves the other; the scroll this causes on
   * the other side mustn't come back. The editor reports it right away
   * (settingEditor), the preview a moment later (ownScroll).
   */
  let settingEditor = false;
  let ownScroll: number | null = null;
  /**
   * The source line at the preview's top, kept as it scrolls: measured
   * after a switch of views, the page would already have reflowed.
   */
  let topLine = 0;

  function setPreviewScroll(top: number) {
    const target = Math.max(0, Math.round(top));
    if (Math.abs(scroller.scrollTop - target) < 1) return;
    scroller.scrollTop = target;
    ownScroll = scroller.scrollTop;
  }

  function syncPreviewToEditor() {
    const editor = props.editor();
    if (editor) scrollPreviewToLine(editorTopLine(editor));
  }

  function scrollEditorToLine(editor: monaco.editor.IStandaloneCodeEditor, line: number) {
    const model = editor.getModel();
    if (!model) return;
    const lineNumber = Math.min(model.getLineCount(), Math.floor(line) + 1);
    const top = editor.getTopForLineNumber(lineNumber);
    const next =
      lineNumber < model.getLineCount()
        ? editor.getTopForLineNumber(lineNumber + 1)
        : top + editor.getOption(monaco.editor.EditorOption.lineHeight);
    settingEditor = true;
    try {
      editor.setScrollTop(top + (line - Math.floor(line)) * (next - top));
    } finally {
      settingEditor = false;
    }
  }

  function onPreviewScroll() {
    const echo = ownScroll !== null && Math.abs(scroller.scrollTop - ownScroll) <= 1;
    ownScroll = null;
    if (echo) return;
    topLine = previewTopLine();
    const editor = props.editor();
    if (props.beside() && editor) scrollEditorToLine(editor, topLine);
  }

  createEffect(() => {
    const editor = props.editor();
    if (!editor) return;
    let frame = 0;
    const sub = editor.onDidScrollChange((e) => {
      if (!e.scrollTopChanged || settingEditor || !props.beside() || frame) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        syncPreviewToEditor();
      });
    });
    onCleanup(() => {
      sub.dispose();
      cancelAnimationFrame(frame);
    });
  });

  // Switching between split and preview reflows the page: keep the same
  // spot in view, and in the split view the editor beside it.
  createEffect(
    on(
      props.beside,
      (beside) => {
        const editor = props.editor();
        const line = beside || !editor ? topLine : editorTopLine(editor);
        requestAnimationFrame(() => {
          boxes = null;
          scrollPreviewToLine(line);
          const ed = props.editor();
          if (beside && ed) {
            ed.layout();
            scrollEditorToLine(ed, line);
          }
        });
      },
      { defer: true }
    )
  );

  onMount(() => {
    props.ref?.(scroller);
    // Images loading, <details> opening, a narrower pane: positions moved.
    // The same line stays at the top.
    const observer = new ResizeObserver(() => {
      boxes = null;
      if (props.beside()) syncPreviewToEditor();
      else scrollPreviewToLine(topLine);
    });
    observer.observe(body);
    observer.observe(scroller);
    onCleanup(() => observer.disconnect());
  });

  // Back to the source alone: it opens where the preview was (unless the
  // whole tab is going: its editor is disposed by then).
  onCleanup(() => {
    const editor = props.editor();
    if (!editor || !rendered) return;
    const line = topLine;
    requestAnimationFrame(() => {
      if (!editor.getModel()) return;
      editor.layout();
      scrollEditorToLine(editor, line);
    });
  });

  // ---------------------------------------------------------------------
  // Headings, links, tasks
  // ---------------------------------------------------------------------

  function findHeading(anchor: string): HTMLElement | null {
    const id = ANCHOR_PREFIX + anchor.replace(/^#/, '');
    const named = [...body.querySelectorAll<HTMLElement>('[id], [name]')];
    for (const name of [id, id.toLowerCase()]) {
      const el = named.find((e) => e.id === name || e.getAttribute('name') === name);
      if (el) return el;
    }
    return null;
  }

  function scrollToHeading(anchor: string) {
    const el = findHeading(anchor);
    if (!el) return;
    const top =
      el.getBoundingClientRect().top - scroller.getBoundingClientRect().top + scroller.scrollTop;
    // The editor follows (onPreviewScroll) when it's beside.
    scroller.scrollTop = Math.max(0, top - 12);
  }

  /** A link from another document to a heading in this one (lib/markdownView.ts). */
  function showRequestedHeading() {
    const anchor = untrack(() => headingRequest(props.path));
    if (anchor === undefined || !rendered) return;
    clearHeadingRequest(props.path);
    scrollToHeading(anchor);
  }
  createEffect(() => {
    if (headingRequest(props.path) !== undefined) showRequestedHeading();
  });

  async function follow(href: string) {
    const link = resolvePreviewLink(href, props.path, props.root());
    if (link.kind === 'anchor') {
      scrollToHeading(link.id);
    } else if (link.kind === 'web') {
      invoke('open_url', { url: link.url }).catch((err) => console.error('Open link:', err));
    } else if (link.kind === 'file') {
      if (link.path === props.path) {
        if (link.anchor) scrollToHeading(link.anchor);
        else if (link.line) {
          scrollPreviewToLine(link.line - 1);
          const editor = props.editor();
          if (editor && props.beside()) scrollEditorToLine(editor, link.line - 1);
        }
        return;
      }
      const file = await resolveProjectFile(link.path);
      if (!file) {
        notify({
          kind: 'warning',
          title: `Can't open ${toRelativePath(link.path, props.root())}`,
          message: 'The link points to a folder or to a file that doesn’t exist.',
          timeout: 6_000,
        });
        return;
      }
      if (link.anchor) requestHeading(file, link.anchor);
      requestOpenFile({ path: file, line: link.line });
    }
  }

  /** Ticks or clears a task's box in the source, as one undoable edit. */
  function toggleTask(box: Element) {
    const editor = props.editor();
    const model = editor?.getModel();
    const item = box.closest('li[data-line]');
    if (!editor || !model || !item || props.readOnly()) return;
    if (editor.getOption(monaco.editor.EditorOption.readOnly)) return;
    const lineNumber = Number(item.getAttribute('data-line')) + 1;
    if (!(lineNumber >= 1 && lineNumber <= model.getLineCount())) return;
    const text = model.getLineContent(lineNumber);
    const toggled = toggleTaskLine(text);
    if (toggled === null) return;
    editor.pushUndoStop();
    editor.executeEdits('markdown-preview', [
      { range: new monaco.Range(lineNumber, 1, lineNumber, text.length + 1), text: toggled },
    ]);
    editor.pushUndoStop();
  }

  function onClick(e: MouseEvent) {
    const target = e.target as Element | null;
    const box = target?.closest?.('input[type="checkbox"]');
    if (box) {
      // The box shows the source: it changes when the source does.
      e.preventDefault();
      toggleTask(box);
      return;
    }
    const anchor = target?.closest?.('a');
    if (!anchor) return;
    // Following a link would replace the whole app in the webview.
    e.preventDefault();
    void follow(anchor.getAttribute('href') ?? anchor.getAttribute('xlink:href') ?? '');
  }

  function onAuxClick(e: MouseEvent) {
    const anchor = (e.target as Element | null)?.closest?.('a');
    if (!anchor || e.button !== 1) return;
    e.preventDefault();
    void follow(anchor.getAttribute('href') ?? '');
  }

  /** In the split view: the editor's cursor to the block double-clicked. */
  function onDblClick(e: MouseEvent) {
    const editor = props.editor();
    const block = (e.target as Element | null)?.closest?.('[data-line]');
    if (!props.beside() || !editor || !block) return;
    const lineNumber = Number(block.getAttribute('data-line')) + 1;
    editor.setPosition({ lineNumber, column: 1 });
    editor.revealLineInCenterIfOutsideViewport(lineNumber);
    editor.focus();
  }

  // Ctrl+S saves, Ctrl+Shift+V and Ctrl+K V switch views, as in the editor.
  let chordUntil = 0;
  function onKeyDown(e: KeyboardEvent) {
    const mod = isMac ? e.metaKey : e.ctrlKey;
    const key = e.key.toLowerCase();
    const chord = chordUntil > Date.now();
    chordUntil = 0;
    if (chord && !mod && !e.altKey && key === 'v') {
      e.preventDefault();
      toggleSplit(props.path);
    } else if (mod && !e.altKey && !e.shiftKey && key === 's') {
      e.preventDefault();
      props.onSave();
    } else if (mod && !e.altKey && e.shiftKey && key === 'v') {
      e.preventDefault();
      togglePreview(props.path);
    } else if (mod && !e.altKey && !e.shiftKey && key === 'k') {
      e.preventDefault();
      chordUntil = Date.now() + 2000;
    } else if (mod && !e.altKey && !e.shiftKey && key === 'a') {
      // Select the document, not the whole app.
      e.preventDefault();
      const range = document.createRange();
      range.selectNodeContents(body);
      const selection = window.getSelection();
      selection?.removeAllRanges();
      selection?.addRange(range);
    }
  }

  return (
    <div
      ref={scroller}
      class="md-preview monaco-component"
      tabIndex={0}
      onScroll={onPreviewScroll}
      onClick={onClick}
      onAuxClick={onAuxClick}
      onDblClick={onDblClick}
      onKeyDown={onKeyDown}
      // A link or image dropped elsewhere in the webview would navigate the app away.
      onDragStart={(e) => e.preventDefault()}
    >
      <article ref={body} class="md-preview-body" />
    </div>
  );
}
