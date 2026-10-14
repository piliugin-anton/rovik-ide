// composerSegments.ts
//
// The chat input's content as segments: its text, and the chips in it (file
// references, blocks of text, images, PDFs). Drafts are kept as segments
// (composerDraft.ts, pendingDraft.ts), copies carry them
// (composerClipboard.ts), and a message's text and attachments are made
// from them.

import type { ChatMessage, FileSuggestion } from '../types';
import type { AttachmentKind } from './attachmentFormats';
import { asPdf, type ComposerPdf } from './composerAttachments';
import { asImage, type ComposerImage } from './composerImages';
import { referenceLabel } from './fileReferences';
import { blockMarkup, blockText, splitBlocks } from './textBlocks';

export type DraftSegment =
  | { type: 'text'; text: string }
  | { type: 'file'; file: FileSuggestion }
  /** A long paste, or a text file attached to the message (`name`), kept as a chip. */
  | { type: 'pasted'; text: string; name?: string }
  /** An image, stored as an attachment (composerImages.ts). */
  | { type: 'image'; image: ComposerImage }
  /** A PDF, stored as an attachment (composerAttachments.ts). */
  | { type: 'pdf'; pdf: ComposerPdf };

/** A segment the input shows as a chip. */
export type ChipSegment = Exclude<DraftSegment, { type: 'text' }>;

const isText = (v: unknown): v is string => typeof v === 'string';

function asFileSuggestion(value: unknown): FileSuggestion | null {
  const file = value as Partial<FileSuggestion> | null;
  if (!file || ![file.id, file.path, file.name, file.relativePath].every(isText)) return null;
  const lines = file.lines;
  if (lines !== undefined && !(Number.isInteger(lines?.start) && Number.isInteger(lines?.end))) {
    return null;
  }
  return file as FileSuggestion;
}

/** The value as a segment, if it is one: what a copy puts on the clipboard, read back. */
export function asSegment(value: unknown): DraftSegment | null {
  const segment = value as Record<string, unknown> | null;
  switch (segment?.type) {
    case 'text':
      return isText(segment.text) ? { type: 'text', text: segment.text } : null;
    case 'pasted':
      if (!isText(segment.text) || !(segment.name === undefined || isText(segment.name))) {
        return null;
      }
      return isText(segment.name)
        ? { type: 'pasted', text: segment.text, name: segment.name }
        : { type: 'pasted', text: segment.text };
    case 'file': {
      const file = asFileSuggestion(segment.file);
      return file ? { type: 'file', file } : null;
    }
    case 'image': {
      const image = asImage(segment.image);
      return image ? { type: 'image', image } : null;
    }
    case 'pdf': {
      const pdf = asPdf(segment.pdf);
      return pdf ? { type: 'pdf', pdf } : null;
    }
    default:
      return null;
  }
}

/** What a segment is as an attachment; null for text, references and pastes. */
export function segmentKind(segment: DraftSegment): AttachmentKind | null {
  if (segment.type === 'image' || segment.type === 'pdf') return segment.type;
  if (segment.type === 'pasted' && segment.name !== undefined) return 'text';
  return null;
}

/**
 * The message's text as stored and sent: file references by their labels,
 * blocks of text in their tags (textBlocks.ts). Images and PDFs aren't
 * text: they go with the message as attachments.
 */
export function messageText(segments: DraftSegment[]): string {
  return segments
    .map((segment) => {
      switch (segment.type) {
        case 'text':
          return segment.text;
        case 'file':
          return referenceLabel(segment.file);
        case 'pasted':
          return blockMarkup(segment);
        default:
          return '';
      }
    })
    .join('');
}

/** A segment for people: a paste in full, an attachment named in brackets. */
function plainPiece(segment: DraftSegment): string {
  switch (segment.type) {
    case 'text':
      return segment.text;
    case 'file':
      return referenceLabel(segment.file);
    case 'pasted':
      return segment.name === undefined ? `\n${segment.text}\n` : blockText(segment);
    case 'image':
      return '[image]';
    case 'pdf':
      return `[${segment.pdf.name}]`;
  }
}

/**
 * The text a copy gives other apps: a paste in full, attachments named as
 * their chips show them ("[notes.md]", "[image]"), apart from the words
 * around them.
 */
export function plainText(segments: DraftSegment[]): string {
  let out = '';
  segments.forEach((segment, i) => {
    const piece = plainPiece(segment);
    const named = segmentKind(segment) !== null || (i > 0 && segmentKind(segments[i - 1]) !== null);
    if (named && /\S$/.test(out) && /^\S/.test(piece)) out += ' ';
    out += piece;
  });
  return out.trim();
}

/** The stored attachments (images, PDFs), in order, once each: what the message sends. */
export function attachmentIds(segments: DraftSegment[]): string[] {
  const ids = segments.flatMap((s) =>
    s.type === 'image' ? [s.image.id] : s.type === 'pdf' ? [s.pdf.id] : []
  );
  return [...new Set(ids)];
}

/** The PDFs among the segments, once each (a message's `meta.pdfs`). */
export function pdfsOf(segments: DraftSegment[]): ComposerPdf[] {
  const seen = new Set<string>();
  return segments.flatMap((s) => {
    if (s.type !== 'pdf' || seen.has(s.pdf.id)) return [];
    seen.add(s.pdf.id);
    return [s.pdf];
  });
}

/**
 * A sent message as segments again, to paste it back as it was written:
 * its text and blocks, then its images and PDFs.
 */
export function segmentsOfMessage(
  m: Pick<ChatMessage, 'content' | 'attachments' | 'meta'>
): DraftSegment[] {
  const parts = splitBlocks(m.content);
  const segments: DraftSegment[] = [];
  parts.forEach((part, i) => {
    if (typeof part !== 'string') {
      segments.push({
        type: 'pasted',
        text: part.text,
        ...(part.name !== undefined ? { name: part.name } : {}),
      });
      return;
    }
    // Blocks go on lines of their own (blockMarkup): those breaks aren't the user's.
    let text = part;
    if (i > 0) text = text.replace(/^\n/, '');
    if (i < parts.length - 1) text = text.replace(/\n$/, '');
    if (text) segments.push({ type: 'text', text });
  });
  const pdfs = new Map((m.meta?.pdfs ?? []).map((pdf) => [pdf.id, pdf]));
  const attached = (m.attachments ?? []).map((id): DraftSegment => {
    const pdf = pdfs.get(id);
    return pdf ? { type: 'pdf', pdf } : { type: 'image', image: { id } };
  });
  if (attached.length > 0 && segments.length > 0) segments.push({ type: 'text', text: '\n' });
  return [...segments, ...attached];
}
