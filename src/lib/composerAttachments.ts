// composerAttachments.ts
//
// Files attached to the chat input, pasted or picked with the attach button:
// each made ready as its kind needs (attachmentFormats.ts) and turned into a
// segment of the input (composerSegments.ts). Images and PDFs are stored as
// chat attachments (chat_db/attachments.rs) and go with the message; a text
// file's text goes in the message itself, like a long paste.

import { chatApi } from './chatApi';
import {
  attachmentKind,
  decodeText,
  MAX_PDF_BYTES,
  MAX_TEXT_FILE_BYTES,
  type AttachmentKind,
} from './attachmentFormats';
import { storeImage } from './composerImages';
import type { ChipSegment } from './composerSegments';
import { formatBytes } from './textUtils';

/** A PDF in the chat input, stored as an attachment (a DraftSegment). */
export interface ComposerPdf {
  /** The attachment id: SHA-256 of the stored bytes. */
  id: string;
  /** The file's name, for the chip and the model. */
  name: string;
  /** Its size in bytes. */
  size: number;
  /** Its pages, as the backend counted them: what it costs a request. */
  pages: number;
}

/** The value as a ComposerPdf, if it is one. */
export function asPdf(value: unknown): ComposerPdf | null {
  const pdf = value as Partial<ComposerPdf> | null;
  if (
    !pdf ||
    typeof pdf.id !== 'string' ||
    !/^[0-9a-f]{64}$/.test(pdf.id) ||
    typeof pdf.name !== 'string' ||
    typeof pdf.size !== 'number' ||
    typeof pdf.pages !== 'number'
  ) {
    return null;
  }
  return { id: pdf.id, name: pdf.name, size: pdf.size, pages: pdf.pages };
}

/** A chip's PDF, from its `data-pdf`; null when it isn't one. */
export function parseComposerPdf(raw: string | undefined): ComposerPdf | null {
  if (!raw) return null;
  try {
    return asPdf(JSON.parse(raw));
  } catch {
    return null;
  }
}

/** A file's name; a pasted image has none. */
export function fileName(file: Blob): string {
  return typeof File !== 'undefined' && file instanceof File ? file.name : '';
}

async function storePdf(file: Blob): Promise<ComposerPdf> {
  if (file.size > MAX_PDF_BYTES) {
    throw new Error(
      `It's ${formatBytes(file.size)}; a PDF can be ${formatBytes(MAX_PDF_BYTES)} at most.`
    );
  }
  const stored = await chatApi.storeAttachment(new Uint8Array(await file.arrayBuffer()));
  if (stored.mime !== 'application/pdf') throw new Error("It isn't a PDF.");
  return {
    id: stored.id,
    name: fileName(file) || 'document.pdf',
    size: file.size,
    pages: stored.pages ?? 1,
  };
}

async function readTextFile(file: Blob): Promise<string> {
  if (file.size > MAX_TEXT_FILE_BYTES) {
    throw new Error(
      `It's ${formatBytes(file.size)}; a text file can be ${formatBytes(MAX_TEXT_FILE_BYTES)} ` +
        'at most. Put it in the project and mention it with @: the agent reads what it needs.'
    );
  }
  const text = decodeText(new Uint8Array(await file.arrayBuffer()));
  if (text === null) {
    throw new Error("It isn't text: only images, PDFs and UTF-8 text files can be attached.");
  }
  return text;
}

/** Makes a file ready to send, as its kind needs; the segment the input shows it as. */
export async function attachmentSegment(file: Blob, kind: AttachmentKind): Promise<ChipSegment> {
  switch (kind) {
    case 'image':
      return { type: 'image', image: await storeImage(file) };
    case 'pdf':
      return { type: 'pdf', pdf: await storePdf(file) };
    case 'text':
      return { type: 'pasted', text: await readTextFile(file), name: fileName(file) || 'text.txt' };
  }
}

/** Files in pasted data (`clipboardData`), in order. */
export function pastedFiles(data: DataTransfer | null): File[] {
  if (!data) return [];
  const items = Array.from(data.items ?? [])
    .filter((item) => item.kind === 'file')
    .map((item) => item.getAsFile())
    .filter((file): file is File => !!file);
  return items.length > 0 ? items : Array.from(data.files ?? []);
}

/**
 * A line naming a copied file: a file:// URL, an absolute path ("/…",
 * "~/…", "C:\…"), or a bare name with no spaces ("shot.png").
 */
const FILE_NAME_LINE = /^(file:\/\/.+|[/~].+|[a-z]:\\.+|[^\s/\\]+)\.[a-z0-9]+$/i;

/**
 * What a paste with both inserts. Text wins: copying from a spreadsheet or
 * a document puts a picture of the cells next to their text, and the text
 * is what's meant. Except when the text only names the files, as files
 * copied in a file manager come with their paths.
 */
export function pasteTakes(text: string, files: number): 'text' | 'files' | 'nothing' {
  const lines = text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
  if (files > 0 && lines.every((line) => FILE_NAME_LINE.test(line))) return 'files';
  if (text) return 'text';
  return 'nothing';
}

/** What the clipboard holds, read through the async clipboard API. */
export interface ClipboardContent {
  text: string;
  images: Blob[];
}

/**
 * Reads the clipboard: its text and the images in a type models take, or
 * only its text where images can't be read. Throws when it can't be read
 * at all.
 */
export async function readClipboard(): Promise<ClipboardContent> {
  const clipboard = navigator.clipboard;
  if (clipboard?.read) {
    try {
      const content: ClipboardContent = { text: '', images: [] };
      for (const item of await clipboard.read()) {
        if (!content.text && item.types.includes('text/plain')) {
          content.text = await (await item.getType('text/plain')).text();
        }
        const image = item.types.find((type) => attachmentKind({ name: '', type }) === 'image');
        if (image) content.images.push(await item.getType(image));
      }
      return content;
    } catch (err) {
      if (!clipboard.readText) throw err;
    }
  }
  if (!clipboard?.readText) throw new Error('The clipboard API is unavailable.');
  return { text: await clipboard.readText(), images: [] };
}
