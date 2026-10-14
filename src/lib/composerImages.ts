// composerImages.ts
//
// Images attached to the chat input (pasted, or picked with the attach
// button): made ready for a model (scaled down, in a type the providers
// take) and stored as chat attachments (chat_db/attachments.rs). The input
// shows each as a chip; the message sends their ids, and the backend sends
// the images themselves to models marked as accepting images.

import { chatApi } from './chatApi';

/** An image in the chat input, stored as an attachment (a DraftSegment). */
export interface ComposerImage {
  /** The attachment id: SHA-256 of the stored bytes. */
  id: string;
  /** Its size, when known: an image copied from a sent message comes without. */
  width?: number;
  height?: number;
}

/** Longest side sent: providers scale bigger images down anyway, and charge by size. */
export const MAX_IMAGE_SIDE = 2048;
/** One image's bytes at most, under the providers' own limits (Anthropic: 5 MB). */
export const MAX_IMAGE_BYTES = 4 * 1024 * 1024;
/** Types sent as they are. A GIF (it can be animated, and Gemini takes none) becomes a PNG. */
const SENT_AS_IS = new Set(['image/png', 'image/jpeg', 'image/webp']);

const isSize = (n: unknown) => n === undefined || (typeof n === 'number' && n > 0);

/** The value as a ComposerImage, if it is one. */
export function asImage(value: unknown): ComposerImage | null {
  const image = value as Partial<ComposerImage> | null;
  if (
    !image ||
    typeof image.id !== 'string' ||
    !/^[0-9a-f]{64}$/.test(image.id) ||
    !isSize(image.width) ||
    !isSize(image.height)
  ) {
    return null;
  }
  return image.width && image.height
    ? { id: image.id, width: image.width, height: image.height }
    : { id: image.id };
}

/** A chip's image, from its `data-image`; null when it isn't one. */
export function parseComposerImage(raw: string | undefined): ComposerImage | null {
  if (!raw) return null;
  try {
    return asImage(JSON.parse(raw));
  } catch {
    return null;
  }
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

/** Makes an image ready to send and stores it; what its chip shows. */
export async function storeImage(blob: Blob): Promise<ComposerImage> {
  const { bytes, width, height } = await prepare(blob);
  const { id } = await chatApi.storeAttachment(bytes);
  return { id, width, height };
}
