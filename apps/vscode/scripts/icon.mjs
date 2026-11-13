// Draws media/icon.png, the extension's icon in the Marketplace and the Extensions view:
// the spark of ognivo (media/ognivo.svg) in its ember on a dark rounded square, 128 px, at
// 4x4 samples a pixel. Written once and kept in git: `node scripts/icon.mjs`.

import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { deflateSync } from "node:zlib";

const SIZE = 128;
const SS = 4;
const here = dirname(fileURLToPath(import.meta.url));

// the spark's star on the 24-unit grid of the SVG, scaled into the square with a margin
const STAR = [[12, 1.5], [14.1, 9.9], [22.5, 12], [14.1, 14.1], [12, 22.5], [9.9, 14.1], [1.5, 12], [9.9, 9.9]];
const DOTS = [[19.4, 4.6, 1.35, 0.85], [4.9, 19.1, 0.85, 0.6]];
const scale = 100 / 24;
const offset = (SIZE - 24 * scale) / 2;
const at = ([x, y]) => [offset + x * scale, offset + y * scale];

function inside(poly, x, y) {
  let hit = false;
  for (let i = 0, j = poly.length - 1; i < poly.length; j = i++) {
    const [xi, yi] = poly[i];
    const [xj, yj] = poly[j];
    if (yi > y !== yj > y && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi) hit = !hit;
  }
  return hit;
}

function inRounded(x, y, r) {
  const m = 4;
  const lo = m + r;
  const hi = SIZE - m - r;
  const cx = Math.min(Math.max(x, lo), hi);
  const cy = Math.min(Math.max(y, lo), hi);
  return x >= m && x <= SIZE - m && y >= m && y <= SIZE - m && (x - cx) ** 2 + (y - cy) ** 2 <= r * r;
}

const star = STAR.map(at);
const dots = DOTS.map(([x, y, r, a]) => [...at([x, y]), r * scale, a]);
const center = at([12, 12]);
const BG = [15, 16, 18];
const EMBER = [240, 136, 62];
const HOT = [255, 248, 236];

const rgba = Buffer.alloc(SIZE * SIZE * 4);
for (let py = 0; py < SIZE; py++) {
  for (let px = 0; px < SIZE; px++) {
    let r = 0, g = 0, b = 0, a = 0;
    for (let sy = 0; sy < SS; sy++) {
      for (let sx = 0; sx < SS; sx++) {
        const x = px + (sx + 0.5) / SS;
        const y = py + (sy + 0.5) / SS;
        if (!inRounded(x, y, 28)) continue;
        let c = BG;
        let o = 1;
        if (inside(star, x, y)) {
          // the radial gradient of the logo: hot at the heart, ember out to the tips
          const d = Math.hypot(x - center[0], y - center[1]) / (11 * scale);
          const t = Math.min(1, d / 0.38);
          const glow = d <= 0.38 ? 1 : Math.max(0.25, 1 - ((d - 0.38) / 0.62) * 0.75);
          const mix = d <= 0.38 ? HOT.map((h, i) => h + (EMBER[i] - h) * t) : EMBER;
          c = mix.map((v, i) => BG[i] + (v - BG[i]) * glow);
        } else {
          for (const [dx, dy, dr, da] of dots) {
            if ((x - dx) ** 2 + (y - dy) ** 2 <= dr * dr) c = EMBER.map((v, i) => BG[i] + (v - BG[i]) * da);
          }
        }
        r += c[0] * o;
        g += c[1] * o;
        b += c[2] * o;
        a += o;
      }
    }
    const n = SS * SS;
    const i = (py * SIZE + px) * 4;
    rgba[i] = a ? Math.round(r / a) : 0;
    rgba[i + 1] = a ? Math.round(g / a) : 0;
    rgba[i + 2] = a ? Math.round(b / a) : 0;
    rgba[i + 3] = Math.round((a / n) * 255);
  }
}

function crc32(buf) {
  let c = ~0;
  for (const byte of buf) {
    c ^= byte;
    for (let k = 0; k < 8; k++) c = (c >>> 1) ^ (0xedb88320 & -(c & 1));
  }
  return ~c >>> 0;
}

function chunk(type, data) {
  const head = Buffer.alloc(8);
  head.writeUInt32BE(data.length, 0);
  head.write(type, 4, "ascii");
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([head.subarray(4), data])), 0);
  return Buffer.concat([head, data, crc]);
}

const raw = Buffer.alloc((SIZE * 4 + 1) * SIZE);
for (let y = 0; y < SIZE; y++) {
  raw[y * (SIZE * 4 + 1)] = 0;
  rgba.copy(raw, y * (SIZE * 4 + 1) + 1, y * SIZE * 4, (y + 1) * SIZE * 4);
}
const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(SIZE, 0);
ihdr.writeUInt32BE(SIZE, 4);
ihdr[8] = 8;
ihdr[9] = 6;
const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", ihdr),
  chunk("IDAT", deflateSync(raw, { level: 9 })),
  chunk("IEND", Buffer.alloc(0)),
]);
writeFileSync(join(here, "../media/icon.png"), png);
console.log(`media/icon.png: ${png.length} bytes`);
