import { createMemo } from 'solid-js';
import { Marked } from 'marked';
import DOMPurify from 'dompurify';

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

// Model output may contain links; open them outside the app webview.
DOMPurify.addHook('afterSanitizeAttributes', (node) => {
  if (node.tagName === 'A' && node.getAttribute('href')) {
    node.setAttribute('target', '_blank');
    node.setAttribute('rel', 'noopener noreferrer');
  }
});

export default function FormattedText(props: FormattedTextProps) {
  const formattedHtml = createMemo(() => {
    const rawHtml = markdown.parse(props.text || '', { async: false }) as string;
    return DOMPurify.sanitize(rawHtml, { ADD_ATTR: ['target'] });
  });

  return <div prop:innerHTML={formattedHtml()} class={`${props.class ?? ''} markdown-body`} />;
}
