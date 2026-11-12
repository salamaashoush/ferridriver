declare const process: {
  readonly platform: string;
  /** The variables the session's `allowEnv` grants, and no others. */
  readonly env: Record<string, string | undefined>;
};

declare module 'node:fs' {
  const filesystem: typeof fs;
  export = filesystem;
}

declare module 'node:fs/promises' {
  const promises: FsPromises;
  export = promises;
}

declare module 'node:os' {
  /** The operating system's release, as `uname -r` prints it. */
  export function release(): string;
}

declare module 'node:path' {
  interface ParsedPath {
    root: string;
    dir: string;
    base: string;
    ext: string;
    name: string;
  }
  interface FormatInputPathObject {
    root?: string | undefined;
    dir?: string | undefined;
    base?: string | undefined;
    ext?: string | undefined;
    name?: string | undefined;
  }
  /** Node's `path` in one flavour: the host's, `posix` or `win32`. */
  interface PlatformPath {
    normalize(path: string): string;
    join(...paths: string[]): string;
    resolve(...paths: string[]): string;
    isAbsolute(path: string): boolean;
    relative(from: string, to: string): string;
    dirname(path: string): string;
    basename(path: string, suffix?: string): string;
    extname(path: string): string;
    readonly sep: '\\' | '/';
    readonly delimiter: ';' | ':';
    parse(path: string): ParsedPath;
    format(pathObject: FormatInputPathObject): string;
    toNamespacedPath(path: string): string;
    readonly posix: PlatformPath;
    readonly win32: PlatformPath;
  }
  const path: PlatformPath;
  export default path;
  export const normalize: PlatformPath['normalize'];
  export const join: PlatformPath['join'];
  export const resolve: PlatformPath['resolve'];
  export const isAbsolute: PlatformPath['isAbsolute'];
  export const relative: PlatformPath['relative'];
  export const dirname: PlatformPath['dirname'];
  export const basename: PlatformPath['basename'];
  export const extname: PlatformPath['extname'];
  export const sep: PlatformPath['sep'];
  export const delimiter: PlatformPath['delimiter'];
  export const parse: PlatformPath['parse'];
  export const format: PlatformPath['format'];
  export const toNamespacedPath: PlatformPath['toNamespacedPath'];
  export const posix: PlatformPath;
  export const win32: PlatformPath;
}

declare module 'node:zlib' {
  export function inflateSync(input: Uint8Array | ArrayBuffer | string, options?: { maxOutputLength?: number }): Buffer;
  export function inflateRawSync(input: Uint8Array | ArrayBuffer | string, options?: { maxOutputLength?: number }): Buffer;
}
