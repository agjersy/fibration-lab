// PNG figure generation for mock runs (deterministic per seed). The capture
// backend writes PNG only by default (IPC.md section 8), so the mock does too:
// a small rasteriser draws the plot into a 16-colour bitmap and encodes it as
// an uncompressed PNG. No text is drawn; the title and labels are in the sidecar.

function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function gauss(rng: () => number): number {
  const u = Math.max(rng(), 1e-9);
  const v = rng();
  return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * v);
}

export interface SimParams {
  n: number;
  tau: number;
  amplitude: number;
  noise: number;
  seed: number;
}

export function simParamsFromArgs(args: string[]): SimParams {
  const p: SimParams = { n: 200, tau: 2.5, amplitude: 1.0, noise: 0.05, seed: 0 };
  for (let i = 0; i < args.length; i++) {
    const a = args[i] ?? "";
    const next = args[i + 1];
    const num = next !== undefined ? Number(next) : NaN;
    if (!Number.isFinite(num)) continue;
    if (a === "--n") p.n = Math.max(5, Math.min(2000, Math.round(num)));
    else if (a === "--tau") p.tau = Math.max(0.05, num);
    else if (a === "--amplitude") p.amplitude = num;
    else if (a === "--noise") p.noise = Math.max(0, num);
    else if (a === "--seed") p.seed = Math.round(num);
  }
  return p;
}

const W = 640;
const H = 440;
const ML = 60;
const MR = 20;
const MT = 40;
const MB = 50;

// Palette indices (4 bits per pixel); 0 is the white background.
const FRAME = 1;
const GRID = 2;
const BLUE = 3;
const ORANGE = 4;
const GREEN = 5;
const GRAY = 6;
const PALETTE = [0xffffff, 0x333333, 0xeeeeee, 0x1f77b4, 0xff7f0e, 0x2ca02c, 0x999999];

class Bitmap {
  readonly px = new Uint8Array(W * H);

  set(x: number, y: number, c: number): void {
    const xi = Math.round(x);
    const yi = Math.round(y);
    if (xi >= 0 && xi < W && yi >= 0 && yi < H) this.px[yi * W + xi] = c;
  }

  disc(cx: number, cy: number, r: number, c: number): void {
    for (let dy = -r; dy <= r; dy++) for (let dx = -r; dx <= r; dx++) if (dx * dx + dy * dy <= r * r) this.set(cx + dx, cy + dy, c);
  }

  /** A line of the given width, drawn as overlapping discs along its length. */
  line(x0: number, y0: number, x1: number, y1: number, c: number, width = 1, dash = 0): void {
    const steps = Math.max(1, Math.ceil(Math.max(Math.abs(x1 - x0), Math.abs(y1 - y0))));
    for (let i = 0; i <= steps; i++) {
      if (dash > 0 && Math.floor(i / dash) % 2 === 1) continue;
      const x = x0 + ((x1 - x0) * i) / steps;
      const y = y0 + ((y1 - y0) * i) / steps;
      if (width <= 1) this.set(x, y, c);
      else this.disc(Math.round(x), Math.round(y), (width - 1) / 2, c);
    }
  }

  /** Plot frame with three horizontal grid lines, as the axes of the real figures have. */
  axes(): void {
    const pw = W - ML - MR;
    const ph = H - MT - MB;
    for (let i = 0; i < 3; i++) {
      const y = MT + ph - (i / 2) * ph;
      this.line(ML, y, ML + pw, y, GRID);
    }
    this.line(ML, MT, ML + pw, MT, FRAME);
    this.line(ML, MT + ph, ML + pw, MT + ph, FRAME);
    this.line(ML, MT, ML, MT + ph, FRAME);
    this.line(ML + pw, MT, ML + pw, MT + ph, FRAME);
  }
}

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c >>> 0;
  }
  return table;
})();

function crc32(bytes: Uint8Array): number {
  let c = 0xffffffff;
  for (const b of bytes) c = (CRC_TABLE[(c ^ b) & 0xff] ?? 0) ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function adler32(bytes: Uint8Array): number {
  let a = 1;
  let b = 0;
  for (const v of bytes) {
    a = (a + v) % 65521;
    b = (b + a) % 65521;
  }
  return ((b << 16) | a) >>> 0;
}

function u32(n: number): number[] {
  return [(n >>> 24) & 0xff, (n >>> 16) & 0xff, (n >>> 8) & 0xff, n & 0xff];
}

function chunk(type: string, data: number[] | Uint8Array): number[] {
  const body = new Uint8Array(4 + data.length);
  for (let i = 0; i < 4; i++) body[i] = type.charCodeAt(i);
  body.set(data, 4);
  return [...u32(data.length), ...body, ...u32(crc32(body))];
}

/** zlib stream of stored (uncompressed) deflate blocks. */
function zlibStored(raw: Uint8Array): Uint8Array {
  const MAX = 0xffff;
  const blocks = Math.max(1, Math.ceil(raw.length / MAX));
  const out = new Uint8Array(2 + raw.length + blocks * 5 + 4);
  let at = 0;
  out[at++] = 0x78;
  out[at++] = 0x01;
  for (let i = 0; i < blocks; i++) {
    const part = raw.subarray(i * MAX, Math.min(raw.length, (i + 1) * MAX));
    out[at++] = i === blocks - 1 ? 1 : 0;
    out[at++] = part.length & 0xff;
    out[at++] = part.length >>> 8;
    out[at++] = ~part.length & 0xff;
    out[at++] = (~part.length >>> 8) & 0xff;
    out.set(part, at);
    at += part.length;
  }
  out.set(u32(adler32(raw)), at);
  return out;
}

function encodePng(bitmap: Bitmap): Uint8Array {
  // Indexed colour, 4 bits per pixel: each row is a filter byte and W/2 bytes.
  const stride = 1 + W / 2;
  const raw = new Uint8Array(stride * H);
  for (let y = 0; y < H; y++) {
    for (let x = 0; x < W; x += 2) {
      raw[y * stride + 1 + x / 2] = ((bitmap.px[y * W + x] ?? 0) << 4) | (bitmap.px[y * W + x + 1] ?? 0);
    }
  }
  const palette: number[] = [];
  for (const rgb of PALETTE) palette.push((rgb >>> 16) & 0xff, (rgb >>> 8) & 0xff, rgb & 0xff);
  return Uint8Array.from([
    ...[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a],
    ...chunk("IHDR", [...u32(W), ...u32(H), 4, 3, 0, 0, 0]),
    ...chunk("PLTE", palette),
    ...chunk("IDAT", zlibStored(raw)),
    ...chunk("IEND", []),
  ]);
}

/** A generated figure file: the bytes as base64 (how the mock keeps binary files) and their count. */
export interface MockPng {
  base64: string;
  size: number;
}

function toPng(bitmap: Bitmap): MockPng {
  const bytes = encodePng(bitmap);
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return { base64: btoa(bin), size: bytes.length };
}

export function decayPng(p: SimParams): { png: MockPng; tauFit: number; rms: number } {
  const rng = mulberry32(p.seed + 1);
  const pw = W - ML - MR;
  const ph = H - MT - MB;
  const tmax = 5 * p.tau;
  const ymax = Math.max(p.amplitude * 1.15, 0.1);
  const ymin = Math.min(-0.15 * Math.abs(p.amplitude), -0.05);
  const sx = (t: number) => ML + (t / tmax) * pw;
  const sy = (y: number) => MT + ph - ((y - ymin) / (ymax - ymin)) * ph;
  const bitmap = new Bitmap();
  bitmap.axes();
  let sq = 0;
  for (let i = 0; i < p.n; i++) {
    const t = (i / (p.n - 1)) * tmax;
    const e = gauss(rng) * p.noise;
    sq += e * e;
    bitmap.disc(Math.round(sx(t)), Math.round(sy(p.amplitude * Math.exp(-t / p.tau) + e)), 2, BLUE);
  }
  const tauFit = p.tau * (1 + (rng() - 0.5) * 0.03);
  const fit = (t: number) => sy(p.amplitude * Math.exp(-t / tauFit));
  for (let i = 0; i < 100; i++) {
    const t0 = (i / 100) * tmax;
    const t1 = ((i + 1) / 100) * tmax;
    bitmap.line(sx(t0), fit(t0), sx(t1), fit(t1), ORANGE, 3);
  }
  return { png: toPng(bitmap), tauFit, rms: Math.sqrt(sq / p.n) };
}

export function residualsPng(p: SimParams): MockPng {
  const rng = mulberry32(p.seed + 7);
  const pw = W - ML - MR;
  const ph = H - MT - MB;
  const tmax = 5 * p.tau;
  const lim = Math.max(p.noise * 3.5, 0.01);
  const sx = (t: number) => ML + (t / tmax) * pw;
  const sy = (y: number) => MT + ph - ((y + lim) / (2 * lim)) * ph;
  const bitmap = new Bitmap();
  bitmap.axes();
  bitmap.line(ML, sy(0), ML + pw, sy(0), GRAY, 1, 4);
  for (let i = 0; i < p.n; i++) {
    const t = (i / (p.n - 1)) * tmax;
    bitmap.disc(Math.round(sx(t)), Math.round(sy(gauss(rng) * p.noise)), 2, GREEN);
  }
  return toPng(bitmap);
}
