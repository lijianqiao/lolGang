import { t } from "./i18n";
// 头像压缩（沿用 1.x）：在界面里把图片裁成正方形画到画布上再导出 WebP，64 像素起压到 800 字节以内，
// 放不下就逐步缩小、降画质，然后交给 Rust（Rust 侧再按文件头和大小检查一遍）。

export interface Compressed {
  /** Base64，不带 data: 前缀。 */
  data: string;
  bytes: number;
}

const AVATAR_MAX_BYTES = 800;

function canvasBlob(canvas: HTMLCanvasElement, quality: number): Promise<Blob> {
  return new Promise((resolve, reject) =>
    canvas.toBlob((b) => (b ? resolve(b) : reject(new Error(t("图片导出失败")))), "image/webp", quality),
  );
}

async function toBase64(blob: Blob): Promise<string> {
  const bytes = new Uint8Array(await blob.arrayBuffer());
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}

/** 头像：取中间的正方形，64 像素起压到 800 字节以内。 */
export async function compressForAvatar(file: Blob): Promise<Compressed> {
  let img: ImageBitmap;
  try {
    img = await createImageBitmap(file);
  } catch {
    throw new Error(t("读不了这张图片，换一张试试"));
  }
  const side = Math.min(img.width, img.height);
  const canvas = document.createElement("canvas");
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error(t("图片处理失败"));
  try {
    for (const s of [64, 56, 48, 40, 32]) {
      canvas.width = s;
      canvas.height = s;
      ctx.imageSmoothingQuality = "high";
      ctx.clearRect(0, 0, s, s);
      ctx.drawImage(img, (img.width - side) / 2, (img.height - side) / 2, side, side, 0, 0, s, s);
      for (const q of [0.8, 0.65, 0.5, 0.35, 0.2]) {
        const blob = await canvasBlob(canvas, q);
        if (blob.size <= AVATAR_MAX_BYTES) return { data: await toBase64(blob), bytes: blob.size };
      }
    }
  } finally {
    img.close();
  }
  throw new Error(t("这张图做不成头像，换一张简单点的"));
}
