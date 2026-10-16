import { createEffect, createMemo } from 'solid-js';
import { Marked } from 'marked';
import DOMPurify from 'dompurify';
import { invoke } from '@tauri-apps/api/core';
import { resolveChatLink } from '../lib/markdownPreview';
import { requestOpenFile, resolveProjectFile } from '../lib/editorBridge';
import { notify } from '../lib/notifications';
import { morphHtml } from '../lib/morphHtml';

interface FormattedTextProps {
  text: string;
  class?: string;
}

/*
 * Full block-level GFM (tables, lists, headings, quotes), not parseInline,
 * which leaves tables as raw pipes. `breaks` keeps single newlines as line
 * breaks, which is what chat text expects. Fenced code never gets here:
 * messageSegments splits it out into CodeBlock first.
 */
const markdown = new Marked({ gfm: true, breaks: true });

// Wide tables scroll inside their own box instead of stretching the message.
markdown.use({
  hooks: {
    postprocess: (html) =>
      html
        .replace(/<table>/g, '<div class="md-table-wrap"><table>')
        .replace(/<\/table>/g, '</table></div>'),
  },
});

// Model output may contain links; they open in the system browser (see
// openLink), never in the app webview.
DOMPurify.addHook('afterSanitizeAttributes', (node) => {
  if (node.tagName === 'A' && node.getAttribute('href')) {
    node.setAttribute('target', '_blank');
    node.setAttribute('rel', 'noopener noreferrer');
  }
});

/*
 * The webview ignores target="_blank" and following the link would replace
 * the IDE, so a click hands web links to the default browser instead, and
 * a link to a file of the project opens it in the editor.
 */
function openLink(e: MouseEvent) {
  if (e.button !== 0 && e.button !== 1) return;
  const anchor = (e.target as Element | null)?.closest?.('a[href]');
  if (!anchor) return;
  e.preventDefault();
  const link = resolveChatLink(anchor.getAttribute('href') ?? '');
  if (link.kind === 'web') {
    invoke('open_url', { url: link.url }).catch((err) => console.error('Open link:', err));
  } else if (link.kind === 'file' && e.button === 0) {
    void openProjectFile(link.path, link.line);
  }
}

async function openProjectFile(path: string, line?: number) {
  // A leading `/` is the project's root on GitHub, where models learned it.
  const file =
    (await resolveProjectFile(path)) ??
    (/^[\\/]/.test(path) ? await resolveProjectFile(path.replace(/^[\\/]+/, '')) : null);
  if (!file) {
    notify({
      kind: 'warning',
      title: `Can't open ${path}`,
      message: 'The link points to a folder or to a file that doesn’t exist.',
      timeout: 6_000,
    });
    return;
  }
  requestOpenFile({ path: file, line });
}

export default function FormattedText(props: FormattedTextProps) {
  const formattedHtml = createMemo(() => {
    const rawHtml = markdown.parse(props.text || '', { async: false }) as string;
    return DOMPurify.sanitize(rawHtml, { ADD_ATTR: ['target'] });
  });

  let el!: HTMLDivElement;
  // Not innerHTML: a streaming reply's text is new on every token, and
  // what's already written keeps its nodes (links stay clickable).
  createEffect(() => morphHtml(el, formattedHtml()));

  return (
    <div
      ref={el}
      class={`${props.class ?? ''} markdown-body`}
      onClick={openLink}
      onAuxClick={openLink}
    />
  );
}
