import { inflateSync } from 'node:zlib';

export interface DecodedPng {
  width: number;
  height: number;
  /** RGBA, four bytes a pixel. */
  data: Uint8Array;
  pixel(x: number, y: number): [number, number, number, number];
}

/** Decode an 8-bit, non-interlaced RGB or RGBA PNG. */
export function decodePng(bytes: Uint8Array): DecodedPng {
  const png = Uint8Array.from(bytes);
  const view = new DataView(png.buffer, png.byteOffset, png.byteLength);
  if (view.getUint32(0) !== 0x89504e47) throw new Error('not a PNG');
  const width = view.getUint32(16);
  const height = view.getUint32(20);
  const depth = png[24];
  const colorType = png[25];
  if (depth !== 8 || (colorType !== 2 && colorType !== 6) || png[28] !== 0)
    throw new Error(`unsupported PNG: depth ${depth}, color type ${colorType}, interlace ${png[28]}`);
  const channels = colorType === 6 ? 4 : 3;
  const chunks: Uint8Array[] = [];
  for (let offset = 8; offset < png.length;) {
    const size = view.getUint32(offset);
    if (view.getUint32(offset + 4) === 0x49444154) chunks.push(png.slice(offset + 8, offset + 8 + size));
    offset += size + 12;
  }
  const compressed = new Uint8Array(chunks.reduce((n, c) => n + c.length, 0));
  let at = 0;
  for (const chunk of chunks) {
    compressed.set(chunk, at);
    at += chunk.length;
  }
  const raw = inflateSync(compressed);
  const stride = width * channels;
  const out = new Uint8Array(height * stride);
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)];
    for (let x = 0; x < stride; x++) {
      const value = raw[y * (stride + 1) + 1 + x];
      const left = x >= channels ? out[y * stride + x - channels] : 0;
      const up = y > 0 ? out[(y - 1) * stride + x] : 0;
      const upLeft = y > 0 && x >= channels ? out[(y - 1) * stride + x - channels] : 0;
      let predicted = 0;
      if (filter === 1) predicted = left;
      else if (filter === 2) predicted = up;
      else if (filter === 3) predicted = (left + up) >> 1;
      else if (filter === 4) {
        const p = left + up - upLeft;
        const pa = Math.abs(p - left), pb = Math.abs(p - up), pc = Math.abs(p - upLeft);
        predicted = pa <= pb && pa <= pc ? left : pb <= pc ? up : upLeft;
      }
      out[y * stride + x] = (value + predicted) & 0xff;
    }
  }
  const pixel = (x: number, y: number): [number, number, number, number] => {
    const i = y * stride + x * channels;
    return [out[i], out[i + 1], out[i + 2], channels === 4 ? out[i + 3] : 255];
  };
  const data = new Uint8Array(width * height * 4);
  for (let y = 0; y < height; y++)
    for (let x = 0; x < width; x++) data.set(pixel(x, y), (y * width + x) * 4);
  return { width, height, data, pixel };
}
