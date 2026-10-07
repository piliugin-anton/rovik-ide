import { createMemo } from 'solid-js';
import { Marked } from 'marked';
import DOMPurify from 'dompurify';
import { invoke } from '@tauri-apps/api/core';

// Extend Solid's HTML attributes for TypeScript recognition
declare module 'solid-js' {
  namespace JSX {
    interface HTMLAttributes<T> {
      'prop:innerHTML'?: string;
    }
  }
}

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
 * the IDE, so a click hands web links to the default browser instead.
 */
function openLink(e: MouseEvent) {
  if (e.button !== 0 && e.button !== 1) return;
  const anchor = (e.target as Element | null)?.closest?.('a[href]');
  if (!anchor) return;
  e.preventDefault();
  const href = (anchor as HTMLAnchorElement).href;
  if (/^(https?:|mailto:)/i.test(href)) {
    invoke('open_url', { url: href }).catch((err) => console.error('Open link:', err));
  }
}

export default function FormattedText(props: FormattedTextProps) {
  const formattedHtml = createMemo(() => {
    const rawHtml = markdown.parse(props.text || '', { async: false }) as string;
    return DOMPurify.sanitize(rawHtml, { ADD_ATTR: ['target'] });
  });

  return (
    <div
      prop:innerHTML={formattedHtml()}
      class={`${props.class ?? ''} markdown-body`}
      onClick={openLink}
      onAuxClick={openLink}
    />
  );
}
