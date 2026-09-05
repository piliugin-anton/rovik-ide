import { createMemo } from 'solid-js';
import { marked } from 'marked';
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

export default function FormattedText(props: FormattedTextProps) {
  const formattedHtml = createMemo(() => {
    const rawHtml = marked.parseInline(props.text || '') as string;
    return DOMPurify.sanitize(rawHtml);
  });

  return <div prop:innerHTML={formattedHtml()} class={`${props.class ?? ''} markdown-body`} />;
}
