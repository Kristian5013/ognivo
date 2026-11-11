// Images the user sends with a message (docs/architecture.md §5.8), as the core takes
// them (ognivo-models image.rs): PNG, JPEG, GIF or BMP, at most 10 MB each and 8 a
// message. The window decodes each one first, so what it cannot read never goes; another
// format (WebP, AVIF, SVG, ...) is drawn anew as PNG, and so is one past what a model's
// projector reads, scaled down to it.

import type { Picture } from "./model.ts";

export const MAX_IMAGES = 8;
export const MAX_BYTES = 10 * 1024 * 1024;
/** The bytes the images of one message may have together. */
export const MAX_MESSAGE_BYTES = 32 * 1024 * 1024;
/** Pixels past which an image is scaled down: Qwen's projector reads up to 4096 tokens of
 * 32×32 pixels, Gemma 4's fewer; more is only bytes. */
export const MAX_PIXELS = 2048 * 2048;
const TAKEN = new Set(["image/png", "image/jpeg", "image/gif", "image/bmp"]);

/** An image ready to go with a message: what the core gets, and how the composer names it. */
export interface Attachment extends Picture {
  name: string;
  /** The length of its bytes. */
  bytes: number;
}

/** How an image of `mime`, `bytes` long and `width`×`height`, goes: as it came, or drawn
 * anew as PNG at `scale`. */
export function plan(
  mime: string,
  bytes: number,
  width: number,
  height: number,
): { redraw: boolean; scale: number } {
  const scale = Math.min(1, Math.sqrt(MAX_PIXELS / Math.max(1, width * height)));
  return { redraw: !TAKEN.has(mime) || scale < 1 || bytes > MAX_BYTES, scale };
}

/** About the tokens the model's projector makes of an image, as the core counts them. */
export function imageTokens(width: number, height: number): number {
  return Math.min(4096, Math.max(256, Math.ceil((width * height) / 1024)));
}

/** `118 KB`, `2.4 MB`, as the core writes sizes. */
export function aboutBytes(n: number): string {
  if (n >= 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  if (n >= 1024) return `${Math.ceil(n / 1024)} KB`;
  return `${n} bytes`;
}

/** `PNG, 1280×720, 118 KB`. */
export function describe(a: Pick<Attachment, "mime" | "width" | "height" | "bytes">): string {
  const format = a.mime.replace(/^image\//, "").toUpperCase();
  return `${format}, ${a.width}×${a.height}, ${aboutBytes(a.bytes)}`;
}

/** A picture the thread holds, as an attachment of a message written anew. */
export function fromPicture(p: Picture, i: number): Attachment {
  const padding = p.data.endsWith("==") ? 2 : p.data.endsWith("=") ? 1 : 0;
  return { ...p, name: `image ${i + 1}`, bytes: Math.floor((p.data.length * 3) / 4) - padding };
}

/** The image files of a paste or a drop. */
export function imageFiles(list: FileList | File[] | null | undefined): File[] {
  return [...(list ?? [])].filter((f) => f.type.startsWith("image/"));
}

/** A file, or a pasted image, as it goes with a message. */
export async function attach(file: Blob, name: string): Promise<Attachment> {
  let bitmap: ImageBitmap;
  try {
    bitmap = await createImageBitmap(file);
  } catch {
    throw new Error(`${name}: the window cannot read it as an image`);
  }
  try {
    const { redraw, scale } = plan(file.type, file.size, bitmap.width, bitmap.height);
    let blob = file;
    let { width, height } = bitmap;
    if (redraw) {
      width = Math.max(1, Math.round(bitmap.width * scale));
      height = Math.max(1, Math.round(bitmap.height * scale));
      const canvas = document.createElement("canvas");
      canvas.width = width;
      canvas.height = height;
      const g = canvas.getContext("2d");
      if (!g) throw new Error(`${name}: the window cannot draw it`);
      g.drawImage(bitmap, 0, 0, width, height);
      blob = await encoded(canvas, "image/png");
      // a photo may stay too large as PNG
      if (blob.size > MAX_BYTES) blob = await encoded(canvas, "image/jpeg", 0.9);
    }
    if (blob.size > MAX_BYTES) {
      throw new Error(`${name}: ${aboutBytes(blob.size)}, over the ${aboutBytes(MAX_BYTES)} an image may have`);
    }
    return { name, mime: blob.type, data: await base64(blob), width, height, bytes: blob.size };
  } finally {
    bitmap.close();
  }
}

function encoded(canvas: HTMLCanvasElement, type: string, quality?: number): Promise<Blob> {
  return new Promise((resolve, reject) =>
    canvas.toBlob((b) => (b ? resolve(b) : reject(new Error("the window could not encode the image"))), type, quality),
  );
}

async function base64(blob: Blob): Promise<string> {
  const url = await new Promise<string>((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result));
    r.onerror = () => reject(r.error ?? new Error("the window could not read the image"));
    r.readAsDataURL(blob);
  });
  return url.slice(url.indexOf(",") + 1);
}
