// attachmentFormats.ts
//
// What can be attached to a chat message, and what a model has to accept to
// get it. The formats are the ones models take through the OpenAI-compatible
// APIs this app talks to:
// - Images: PNG, JPEG, WebP and GIF, as `image_url` parts (OpenAI,
//   Anthropic, Gemini, OpenRouter, local vision models). Each is stored as
//   a PNG at most 768 pixels a side (chat_db/images.rs): Gemini takes no
//   GIF, and an animation keeps its first frame, the only one models look
//   at anyway. Sent to models marked Vision.
// - PDF, as a `file` part. OpenAI's Chat Completions take PDFs, and of
//   documents only PDFs (Word, Excel and the like only on its Responses
//   API); OpenRouter takes them for every model, reading out the text where
//   the model can't; Anthropic's API (spoken natively, not through its
//   OpenAI-compatible endpoint) takes them as documents. Gemini's
//   OpenAI-compatible endpoint leaves them out, so, as for Vision, a model
//   is marked as taking PDFs (Anthropic's model list says which do).
// - Text (code, Markdown, CSV, JSON, logs…): every model reads it. A text
//   file goes in the message itself, like a long paste.
// Anything else (Word, spreadsheets, archives, audio, video) no model gets
// through these APIs, so it isn't attached.

import { extensionOf, isLikelyBinary } from './language';

export type AttachmentKind = 'image' | 'pdf' | 'text';

/** What a model accepts besides text: its Vision and PDF settings. */
export interface ModelInputs {
  vision: boolean;
  pdf: boolean;
}

/** Attachments one message can carry, whatever their kind. */
export const MAX_MESSAGE_ATTACHMENTS = 10;
/** One PDF at most: under the 32 MB a request OpenAI and Anthropic take. */
export const MAX_PDF_BYTES = 20 * 1024 * 1024;
/** One image file at most, as attached: it's stored scaled down (chat_db/images.rs). */
export const MAX_IMAGE_BYTES = 50 * 1024 * 1024;
/**
 * One text file at most: about 300,000 tokens, more than most models'
 * whole context. Bigger files belong in the project, read by the agent.
 */
export const MAX_TEXT_FILE_BYTES = 1024 * 1024;

const IMAGE_TYPES = new Set(['image/png', 'image/jpeg', 'image/webp', 'image/gif']);
const IMAGE_EXTENSIONS = new Set(['png', 'jpg', 'jpeg', 'webp', 'gif']);
const PDF_TYPE = 'application/pdf';

/**
 * Text and code files by extension: offered in the file picker, and taken
 * as text whatever type the system gives them (a .ts file is "video/mp2t"
 * to some). Files with other names are text when they read as text.
 */
const TEXT_EXTENSIONS = new Set(
  (
    'txt text md markdown mdx rst adoc org tex bib csv tsv json jsonc json5 jsonl ndjson ' +
    'geojson ipynb xml xsd xsl svg html htm xhtml css scss sass less yaml yml toml ini cfg ' +
    'conf properties env log diff patch sql graphql gql proto js mjs cjs jsx ts mts cts tsx ' +
    'vue svelte astro py pyi rb php java kt kts scala groovy gradle go mod sum rs c h cc cpp ' +
    'cxx hpp hh hxx m mm cs fs fsx vb swift dart lua pl pm r jl hs elm ex exs erl hrl clj ' +
    'cljs edn el scm rkt ml mli nim zig sol sh bash zsh fish ps1 psm1 bat cmd mk cmake ' +
    'dockerfile tf tfvars hcl nix lock srt vtt'
  ).split(' ')
);

/** Types the system gives some text files, beyond text/*. */
const TEXT_TYPES = new Set([
  'application/json',
  'application/ld+json',
  'application/xml',
  'application/yaml',
  'application/x-yaml',
  'application/toml',
  'application/javascript',
  'application/x-javascript',
  'application/typescript',
  'application/x-sh',
  'application/x-shellscript',
  'application/sql',
  'application/x-httpd-php',
  'application/x-ndjson',
  'image/svg+xml',
]);

/** Documents worth a word of their own: they can be saved as PDF. */
const OFFICE_EXTENSIONS = new Set(
  'doc docx docm odt rtf pages xls xlsx xlsm ods numbers ppt pptx odp key epub'.split(' ')
);

/** The file picker's `accept`: every format that can be attached. */
export const ATTACH_ACCEPT = [
  ...IMAGE_TYPES,
  ...[...IMAGE_EXTENSIONS].map((e) => `.${e}`),
  PDF_TYPE,
  '.pdf',
  'text/*',
  ...[...TEXT_EXTENSIONS].map((e) => `.${e}`),
].join(',');

/** A file as far as attaching is concerned: its name (may be empty) and type. */
export interface FileLike {
  name: string;
  type: string;
}

/**
 * What a file is as an attachment, by its type and name; null when it can't
 * be attached. A 'text' file is only known to be text once it's read
 * (`decodeText`): a file the system has no type for may still be binary.
 */
export function attachmentKind(file: FileLike): AttachmentKind | null {
  const type = file.type.toLowerCase().split(';')[0].trim();
  const extension = extensionOf(file.name);
  if (IMAGE_TYPES.has(type) || IMAGE_EXTENSIONS.has(extension)) return 'image';
  if (type === PDF_TYPE || extension === 'pdf') return 'pdf';
  if (TEXT_EXTENSIONS.has(extension) || type.startsWith('text/') || TEXT_TYPES.has(type)) {
    return 'text';
  }
  if (OFFICE_EXTENSIONS.has(extension) || isLikelyBinary(file.name)) return null;
  // No type the system knows (a Makefile, a .env, an unusual extension).
  return type === '' || type === 'application/octet-stream' ? 'text' : null;
}

/** Whether the model takes this kind: images need Vision, PDFs need PDF. */
export function modelTakes(model: ModelInputs, kind: AttachmentKind): boolean {
  if (kind === 'image') return model.vision;
  if (kind === 'pdf') return model.pdf;
  return true;
}

/** Why a file can't be attached at all (attachmentKind gave null). */
export function unsupportedReason(file: FileLike): string {
  const name = file.name ? `“${file.name}”` : 'This file';
  if (OFFICE_EXTENSIONS.has(extensionOf(file.name))) {
    return `${name} can't be sent to a model: save it as PDF first.`;
  }
  return `${name} can't be attached: models take images (PNG, JPEG, WebP, GIF), PDFs and text files.`;
}

/**
 * A text file's content: UTF-8 with no NUL character, line breaks as \n.
 * Null when it isn't text.
 */
export function decodeText(bytes: Uint8Array): string | null {
  let text: string;
  try {
    text = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  } catch {
    return null;
  }
  return text.includes('\0') ? null : text.replace(/\r\n?/g, '\n');
}
