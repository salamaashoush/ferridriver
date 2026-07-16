// Zip central-directory reader for trace/artifact assertions. Lists
// entry metadata and extracts STORED (method 0) entries; DEFLATE
// payloads have no decompressor in the QuickJS sandbox, so tests
// assert on entry names/sizes for compressed content.

export interface ZipEntry {
  name: string;
  method: number;
  compressedSize: number;
  uncompressedSize: number;
  headerOffset: number;
}

const EOCD_SIGNATURE = 0x06054b50;
const CENTRAL_SIGNATURE = 0x02014b50;
const LOCAL_SIGNATURE = 0x04034b50;

function findEocd(view: DataView): number {
  // EOCD is at least 22 bytes from the end; the comment can push it up
  // to 65557 bytes back.
  const start = Math.max(0, view.byteLength - 22 - 65535);
  for (let i = view.byteLength - 22; i >= start; i--) {
    if (view.getUint32(i, true) === EOCD_SIGNATURE) {
      return i;
    }
  }
  throw new Error('zip: end-of-central-directory record not found');
}

export function listZipEntries(bytes: Uint8Array): ZipEntry[] {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const eocd = findEocd(view);
  const count = view.getUint16(eocd + 10, true);
  let offset = view.getUint32(eocd + 16, true);

  const entries: ZipEntry[] = [];
  const decoder = new TextDecoder();
  for (let i = 0; i < count; i++) {
    if (view.getUint32(offset, true) !== CENTRAL_SIGNATURE) {
      throw new Error(`zip: bad central directory signature at ${offset}`);
    }
    const method = view.getUint16(offset + 10, true);
    const compressedSize = view.getUint32(offset + 20, true);
    const uncompressedSize = view.getUint32(offset + 24, true);
    const nameLength = view.getUint16(offset + 28, true);
    const extraLength = view.getUint16(offset + 30, true);
    const commentLength = view.getUint16(offset + 32, true);
    const headerOffset = view.getUint32(offset + 42, true);
    const name = decoder.decode(bytes.subarray(offset + 46, offset + 46 + nameLength));
    entries.push({ name, method, compressedSize, uncompressedSize, headerOffset });
    offset += 46 + nameLength + extraLength + commentLength;
  }
  return entries;
}

// Extract a STORED entry's bytes. Throws for compressed entries.
export function readStoredEntry(bytes: Uint8Array, entry: ZipEntry): Uint8Array {
  if (entry.method !== 0) {
    throw new Error(`zip: entry ${entry.name} uses compression method ${entry.method}; only STORED is readable here`);
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (view.getUint32(entry.headerOffset, true) !== LOCAL_SIGNATURE) {
    throw new Error(`zip: bad local header signature for ${entry.name}`);
  }
  const nameLength = view.getUint16(entry.headerOffset + 26, true);
  const extraLength = view.getUint16(entry.headerOffset + 28, true);
  const dataStart = entry.headerOffset + 30 + nameLength + extraLength;
  return bytes.subarray(dataStart, dataStart + entry.compressedSize);
}
