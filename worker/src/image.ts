// Picture copies: an uploaded JPEG, PNG or WebP shrunk to a width and
// re-encoded as WebP, for phones and narrow columns. Bun.Image decodes and
// encodes on a worker thread with its own static codecs; nothing here
// touches the network or the disk.

/// Largest picture accepted, in bytes; the engine refuses bigger ones first.
export const IMAGE_MAX = 25 * 1024 * 1024;
/// The widths the engine asks for.
const WIDTHS = new Set([480, 960, 1600]);
/// A file claiming more pixels than this is refused before it is decoded,
/// so a small file cannot unpack into gigabytes. 50 megapixels.
const MAX_PIXELS = 50_000_000;

export class ImageError extends Error {}

export async function shrink(bytes: Uint8Array, width: number): Promise<Uint8Array> {
  if (!WIDTHS.has(width)) {
    throw new ImageError(`width must be one of ${[...WIDTHS].join(", ")}`);
  }
  if (bytes.byteLength === 0 || bytes.byteLength > IMAGE_MAX) {
    throw new ImageError("the picture is empty or too large");
  }
  try {
    const image = new Bun.Image(bytes, { maxPixels: MAX_PIXELS });
    const meta = await image.metadata();
    if (!["jpeg", "png", "webp"].includes(meta.format)) {
      throw new ImageError(`not a JPEG, PNG or WebP picture (${meta.format})`);
    }
    return await new Bun.Image(bytes, { maxPixels: MAX_PIXELS })
      .resize(width, undefined, { fit: "inside", withoutEnlargement: true })
      .webp({ quality: 80 })
      .bytes();
  } catch (err) {
    if (err instanceof ImageError) throw err;
    const code = (err as { code?: string }).code ?? "";
    // A picture the codecs cannot read is the file's fault, not the worker's.
    if (code.startsWith("ERR_IMAGE_")) throw new ImageError(code);
    throw err;
  }
}
