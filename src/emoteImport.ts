import { t } from "./i18n";
// 导入表情前在控制面板里把文件准备好：图片取第一帧缩成 160 像素以内、12 KB 以内的静态 WebP（不行就 128 像素再试），
// 动的 GIF、WebP 原样留作动画；音效检查类型、大小、长度。Rust 写盘前还会按文件头再查一遍。
// 移植自 lolPing src/renderer/settings/emoteImport.ts（MIT，© 2026 Danrui Meng）。

const STILL_MAX_BYTES = 12 * 1024;
const STILL_SIDES = [160, 128];
const QUALITIES = [0.85, 0.75, 0.65, 0.55, 0.45];
/** 选来的图片最多这么大、边长最多这么长：一个小文件可以在头里谎报 30000×30000，解码要几个 GB。 */
const INPUT_MAX_BYTES = 8 * 1024 * 1024;
const INPUT_MAX_SIDE = 8192;
const ANIM_MAX_BYTES = 5 * 1024 * 1024;
const ANIM_MAX_SIDE = 1024;
const SOUND_MAX_BYTES = 1024 * 1024;
const SOUND_MAX_S = 5;

type ImageType = "png" | "jpeg" | "gif" | "webp";
const MIME: Record<ImageType, string> = { png: "image/png", jpeg: "image/jpeg", gif: "image/gif", webp: "image/webp" };

export interface PreparedEmote {
  name: string;
  still: Uint8Array;
  anim: Uint8Array | null;
}

export type Result<T> = { ok: true; value: T } | { ok: false; error: string };
const fail = <T>(error: string): Result<T> => ({ ok: false, error });

const ascii = (b: Uint8Array, from: number, to: number): string => String.fromCharCode(...b.subarray(from, to));

/** 按文件头认图片类型，不看扩展名。 */
export function sniffImage(b: Uint8Array): ImageType | null {
  if (b.length >= 8 && b[0] === 0x89 && ascii(b, 1, 4) === "PNG") return "png";
  if (b.length >= 3 && b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff) return "jpeg";
  if (ascii(b, 0, 6) === "GIF87a" || ascii(b, 0, 6) === "GIF89a") return "gif";
  if (b.length >= 12 && ascii(b, 0, 4) === "RIFF" && ascii(b, 8, 12) === "WEBP") return "webp";
  return null;
}

const u16 = (b: Uint8Array, at: number): number => b[at] | (b[at + 1] << 8);
const u24 = (b: Uint8Array, at: number): number => b[at] | (b[at + 1] << 8) | (b[at + 2] << 16);
const be16 = (b: Uint8Array, at: number): number => (b[at] << 8) | b[at + 1];
const be32 = (b: Uint8Array, at: number): number => ((b[at] << 24) | (b[at + 1] << 16) | (b[at + 2] << 8) | b[at + 3]) >>> 0;
const sized = (width: number, height: number): { width: number; height: number } | null =>
  width > 0 && height > 0 ? { width, height } : null;

/** 跳过 GIF 的一串数据子块（每块先是长度，0 结束），返回后面的位置。 */
function skipSubBlocks(b: Uint8Array, at: number): number {
  while (at < b.length && b[at] !== 0) at += b[at] + 1;
  return at + 1;
}

/**
 * 不解码、只读文件头得到的像素大小，也就是解码后有多大。头缺了、截断了、写着 0 时返回 null（量不出来的也拒绝）。
 * GIF 的帧可以超出逻辑屏幕，所以把每一帧都算进去。做法同 lolPing src/shared/imageSniff.ts。
 */
export function imageSize(b: Uint8Array): { width: number; height: number } | null {
  switch (sniffImage(b)) {
    case "png":
      return b.length >= 24 && ascii(b, 12, 16) === "IHDR" ? sized(be32(b, 16), be32(b, 20)) : null;
    case "jpeg": {
      let i = 2;
      while (i < b.length - 1) {
        if (b[i] !== 0xff || b[i + 1] === 0xff) {
          i++;
          continue;
        }
        const marker = b[i + 1];
        if (marker === 0x00 || marker === 0x01 || (marker >= 0xd0 && marker <= 0xd9)) {
          i += 2;
          continue;
        }
        if (marker === 0xda || i + 4 > b.length) return null; // 先到了图像数据：没有帧头
        const length = be16(b, i + 2);
        if (length < 2) return null;
        if (marker >= 0xc0 && marker <= 0xcf && marker !== 0xc4 && marker !== 0xc8 && marker !== 0xcc) {
          return length >= 8 && i + 9 <= b.length ? sized(be16(b, i + 7), be16(b, i + 5)) : null;
        }
        i += 2 + length;
      }
      return null;
    }
    case "gif": {
      if (b.length < 10) return null;
      let width = u16(b, 6);
      let height = u16(b, 8);
      let at = 13;
      if ((b[10] & 0x80) !== 0) at += 3 << ((b[10] & 7) + 1); // 全局颜色表
      while (at < b.length) {
        if (b[at] === 0x21) at = skipSubBlocks(b, at + 2);
        else if (b[at] === 0x2c && at + 10 <= b.length) {
          width = Math.max(width, u16(b, at + 1) + u16(b, at + 5));
          height = Math.max(height, u16(b, at + 3) + u16(b, at + 7));
          const packed = b[at + 9];
          at += 10;
          if ((packed & 0x80) !== 0) at += 3 << ((packed & 7) + 1); // 局部颜色表
          at = skipSubBlocks(b, at + 1);
        } else break;
      }
      return sized(width, height);
    }
    case "webp": {
      const chunk = ascii(b, 12, 16);
      if (chunk === "VP8 ") {
        return b.length >= 30 && b[23] === 0x9d && b[24] === 0x01 && b[25] === 0x2a ? sized(u16(b, 26) & 0x3fff, u16(b, 28) & 0x3fff) : null;
      }
      if (chunk === "VP8L") {
        if (b.length < 25 || b[20] !== 0x2f) return null;
        const bits = (b[21] | (b[22] << 8) | (b[23] << 16) | (b[24] << 24)) >>> 0;
        return { width: (bits & 0x3fff) + 1, height: ((bits >>> 14) & 0x3fff) + 1 };
      }
      return chunk === "VP8X" && b.length >= 30 ? { width: u24(b, 24) + 1, height: u24(b, 27) + 1 } : null;
    }
    default:
      return null;
  }
}

/** 长边缩到 side（不放大），至少 1 像素。 */
export function fitSize(w: number, h: number, side: number): { width: number; height: number } {
  const scale = Math.min(1, side / Math.max(w, h));
  return { width: Math.max(1, Math.round(w * scale)), height: Math.max(1, Math.round(h * scale)) };
}

/** 去掉扩展名当默认名字。 */
export const nameFromFile = (fileName: string): string => fileName.replace(/(.)\.[^.]*$/, "$1");

async function encodeStill(frame: VideoFrame): Promise<Uint8Array | null> {
  for (const side of STILL_SIDES) {
    const size = fitSize(frame.displayWidth, frame.displayHeight, side);
    const canvas = new OffscreenCanvas(size.width, size.height);
    const ctx = canvas.getContext("2d");
    if (!ctx) return null;
    ctx.imageSmoothingQuality = "high";
    ctx.drawImage(frame, 0, 0, size.width, size.height);
    for (const quality of QUALITIES) {
      const blob = await canvas.convertToBlob({ type: "image/webp", quality });
      if (blob.type === "image/webp" && blob.size <= STILL_MAX_BYTES) return new Uint8Array(await blob.arrayBuffer());
    }
  }
  return null;
}

/** 选来的图片 → 静态图（和动画）。 */
export async function prepareEmote(file: File): Promise<Result<PreparedEmote>> {
  if (file.size > INPUT_MAX_BYTES) return fail(t("图片太大了，最多 8 MB"));
  const bytes = new Uint8Array(await file.arrayBuffer());
  const type = sniffImage(bytes);
  if (!type) return fail(t("只能导入 PNG、JPEG、GIF 或 WebP 图片"));
  const declared = imageSize(bytes);
  if (!declared) return fail(t("这张图打不开"));
  if (Math.max(declared.width, declared.height) > INPUT_MAX_SIDE) return fail(t("图片太大了，边长最多 8192 像素"));
  let decoder: ImageDecoder | null = null;
  let frame: VideoFrame | null = null;
  try {
    decoder = new ImageDecoder({ data: bytes, type: MIME[type] });
    await Promise.all([decoder.tracks.ready, decoder.completed]); // 数据都进来了，帧数才准
    const frames = decoder.tracks.selectedTrack?.frameCount ?? 1;
    frame = (await decoder.decode({ frameIndex: 0 })).image;
    const animated = (type === "gif" || type === "webp") && frames > 1;
    if (animated && (bytes.length > ANIM_MAX_BYTES || Math.max(frame.displayWidth, frame.displayHeight) > ANIM_MAX_SIDE)) {
      return fail(t("动图太大了：最多 5 MB、边长 1024 像素"));
    }
    const still = await encodeStill(frame);
    if (!still) return fail(t("这张图细节太多，缩不到 12 KB，换一张简单点的"));
    return { ok: true, value: { name: nameFromFile(file.name), still, anim: animated ? bytes : null } };
  } catch {
    return fail(t("这张图打不开"));
  } finally {
    frame?.close();
    decoder?.close();
  }
}

/** 按文件头认音效类型：OGG、WAV、MP3。 */
export function sniffAudio(b: Uint8Array): boolean {
  if (ascii(b, 0, 4) === "OggS") return true;
  if (b.length >= 12 && ascii(b, 0, 4) === "RIFF" && ascii(b, 8, 12) === "WAVE") return true;
  return ascii(b, 0, 3) === "ID3" || (b.length >= 2 && b[0] === 0xff && (b[1] & 0xe0) === 0xe0);
}

/** 选来的音效：OGG、MP3 或 WAV，1 MB、5 秒以内。 */
export async function prepareSound(file: File): Promise<Result<Uint8Array>> {
  const head = new Uint8Array(await file.slice(0, 16).arrayBuffer());
  if (!sniffAudio(head)) return fail(t("音效只能是 OGG、MP3 或 WAV"));
  if (file.size > SOUND_MAX_BYTES) return fail(t("音效太大了，最多 1 MB"));
  const bytes = new Uint8Array(await file.arrayBuffer());
  try {
    const decoded = await new OfflineAudioContext(1, 1, 44100).decodeAudioData(bytes.slice().buffer); // 解码会拿走缓冲区，给它一份拷贝
    if (decoded.duration > SOUND_MAX_S + 0.05) return fail(t("音效太长了，最多 5 秒"));
    return { ok: true, value: bytes };
  } catch {
    return fail(t("这个音效打不开"));
  }
}

/** 大文件转 base64 不能一次展开成参数（会爆栈），分段转。 */
export function toBase64(b: Uint8Array): string {
  let s = "";
  for (let i = 0; i < b.length; i += 0x8000) s += String.fromCharCode(...b.subarray(i, i + 0x8000));
  return btoa(s);
}
