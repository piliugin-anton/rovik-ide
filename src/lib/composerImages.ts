// composerImages.ts
//
// Images pasted into the chat input: taken from the clipboard, made ready
// for a model (scaled down, in a type the providers take) and stored as
// chat attachments (chat_db/attachments.rs). The input shows each as a
// chip; the message sends their ids, and the backend sends the images
// themselves to models marked as accepting images.

import { chatApi } from './chatApi';

/** An image in the chat input, stored as an attachment (a DraftSegment). */
export interface ComposerImage {
  /** The attachment id: SHA-256 of the stored bytes. */
  id: string;
  width: number;
  height: number;
}

/** Images one message can carry (the backend allows a few more). */
export const MAX_MESSAGE_IMAGES = 10;
/** Longest side sent: providers scale bigger images down anyway, and charge by size. */
export const MAX_IMAGE_SIDE = 2048;
/** One image's bytes at most, under the providers' own limits (Anthropic: 5 MB). */
export const MAX_IMAGE_BYTES = 4 * 1024 * 1024;
/** Types sent as they are. Others (GIF, which can be animated; BMP, TIFF…) become PNG. */
const SENT_AS_IS = new Set(['image/png', 'image/jpeg', 'image/webp']);

/** A chip's image, from its `data-image`; null when it isn't one. */
export function parseComposerImage(raw: string | undefined): ComposerImage | null {
  if (!raw) return null;
  try {
    const value = JSON.parse(raw) as Partial<ComposerImage> | null;
    if (
      value &&
      typeof value.id === 'string' &&
      /^[0-9a-f]{64}$/.test(value.id) &&
      typeof value.width === 'number' &&
      typeof value.height === 'number'
    ) {
      return { id: value.id, width: value.width, height: value.height };
    }
  } catch {
    // Not an image chip's payload.
  }
  return null;
}

/** The size to scale to: the longest side at most `max`, the shape kept. */
export function fittedSize(
  width: number,
  height: number,
  max = MAX_IMAGE_SIDE
): { width: number; height: number } {
  const longest = Math.max(width, height);
  if (longest <= max) return { width, height };
  const scale = max / longest;
  return {
    width: Math.max(1, Math.round(width * scale)),
    height: Math.max(1, Math.round(height * scale)),
  };
}

/** Image files in pasted data (`clipboardData`), in order. */
export function imageFiles(data: DataTransfer | null): File[] {
  if (!data) return [];
  const items = Array.from(data.items ?? [])
    .filter((item) => item.kind === 'file' && item.type.startsWith('image/'))
    .map((item) => item.getAsFile())
    .filter((file): file is File => !!file);
  if (items.length > 0) return items;
  return Array.from(data.files ?? []).filter((file) => file.type.startsWith('image/'));
}

/**
 * A copied image file's path or name: a file:// URL, an absolute path
 * ("/…", "~/…", "C:\…"), or a bare name with no spaces ("shot.png").
 */
const IMAGE_FILE_PATH =
  /^(file:\/\/.+|[/~].+|[a-z]:\\.+|[^\s/\\]+)\.(png|jpe?g|gif|webp|bmp|tiff?|heic|avif)$/i;

/**
 * What a paste with both inserts. Text wins: copying from a spreadsheet or
 * a document puts a picture of the cells next to their text, and the text
 * is what's meant. Except when the text only names the image, as a file
 * copied in a file manager comes with its path.
 */
export function pasteTakes(text: string, images: number): 'text' | 'images' | 'nothing' {
  const line = text.trim();
  if (images > 0 && (!line || (!line.includes('\n') && IMAGE_FILE_PATH.test(line)))) {
    return 'images';
  }
  if (text) return 'text';
  return 'nothing';
}

/** What the clipboard holds, read through the async clipboard API. */
export interface ClipboardContent {
  text: string;
  images: Blob[];
}

/**
 * Reads the clipboard: its text and images, or only its text where images
 * can't be read. Throws when it can't be read at all.
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
        const image = item.types.find((t) => t.startsWith('image/'));
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

function canvasBlob(canvas: HTMLCanvasElement, type: string, quality?: number): Promise<Blob> {
  return new Promise((resolve, reject) =>
    canvas.toBlob(
      (blob) => (blob ? resolve(blob) : reject(new Error("The image couldn't be converted."))),
      type,
      quality
    )
  );
}

/**
 * The image's bytes as they're sent: unchanged when it's fine as it is,
 * else scaled to MAX_IMAGE_SIDE and encoded as PNG, or as JPEG when even
 * that is over MAX_IMAGE_BYTES.
 */
async function prepare(blob: Blob): Promise<{ bytes: Uint8Array; width: number; height: number }> {
  let bitmap: ImageBitmap;
  try {
    bitmap = await createImageBitmap(blob);
  } catch {
    throw new Error("It isn't an image this app can read.");
  }
  try {
    const { width, height } = bitmap;
    const size = fittedSize(width, height);
    const scaled = size.width !== width || size.height !== height;
    if (!scaled && SENT_AS_IS.has(blob.type) && blob.size <= MAX_IMAGE_BYTES) {
      return { bytes: new Uint8Array(await blob.arrayBuffer()), width, height };
    }
    const canvas = document.createElement('canvas');
    canvas.width = size.width;
    canvas.height = size.height;
    const context = canvas.getContext('2d');
    if (!context) throw new Error("The image couldn't be converted.");
    context.drawImage(bitmap, 0, 0, size.width, size.height);
    let out = await canvasBlob(canvas, 'image/png');
    if (out.size > MAX_IMAGE_BYTES) {
      // JPEG has no transparency: what was see-through becomes white, not black.
      context.globalCompositeOperation = 'destination-over';
      context.fillStyle = '#fff';
      context.fillRect(0, 0, size.width, size.height);
      out = await canvasBlob(canvas, 'image/jpeg', 0.9);
    }
    if (out.size > MAX_IMAGE_BYTES) throw new Error('The image is too large, even scaled down.');
    return { bytes: new Uint8Array(await out.arrayBuffer()), ...size };
  } finally {
    bitmap.close();
  }
}

/** Makes a pasted image ready to send and stores it; what its chip shows. */
export async function storePastedImage(blob: Blob): Promise<ComposerImage> {
  const { bytes, width, height } = await prepare(blob);
  const id = await chatApi.storeImage(bytes);
  return { id, width, height };
}
