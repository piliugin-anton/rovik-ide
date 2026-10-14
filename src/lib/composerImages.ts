// composerImages.ts
//
// Images attached to the chat input (pasted, or picked with the attach
// button), stored as chat attachments: the backend reads the file as it
// comes and keeps a PNG at most 768 pixels a side, the right way up
// (chat_db/images.rs). The input shows each as a chip; the message sends
// their ids, and the backend sends the images themselves to models marked
// as accepting images.

import { MAX_IMAGE_BYTES } from './attachmentFormats';
import { chatApi } from './chatApi';
import { formatBytes } from './textUtils';

/** An image in the chat input, stored as an attachment (a DraftSegment). */
export interface ComposerImage {
  /** The attachment id: SHA-256 of the stored bytes. */
  id: string;
  /** Its size, when known: an image copied from a sent message comes without. */
  width?: number;
  height?: number;
}

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

/** Stores an image as the backend makes it ready to send; what its chip shows. */
export async function storeImage(file: Blob): Promise<ComposerImage> {
  if (file.size > MAX_IMAGE_BYTES) {
    throw new Error(
      `It's ${formatBytes(file.size)}; an image can be ${formatBytes(MAX_IMAGE_BYTES)} at most.`
    );
  }
  const stored = await chatApi.storeAttachment(new Uint8Array(await file.arrayBuffer()));
  if (!stored.width || !stored.height) throw new Error("It isn't an image.");
  return { id: stored.id, width: stored.width, height: stored.height };
}
