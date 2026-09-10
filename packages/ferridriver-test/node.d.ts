declare const process: {
  readonly platform: string;
};

declare module 'node:fs' {
  const filesystem: typeof fs;
  export = filesystem;
}

declare module 'node:fs/promises' {
  const promises: FsPromises;
  export = promises;
}

declare module 'node:zlib' {
  export function inflateSync(input: Uint8Array | ArrayBuffer | string, options?: { maxOutputLength?: number }): Buffer;
  export function inflateRawSync(input: Uint8Array | ArrayBuffer | string, options?: { maxOutputLength?: number }): Buffer;
}
