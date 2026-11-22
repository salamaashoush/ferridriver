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

declare module 'node:child_process' {
  type StdioOption = 'pipe' | 'inherit' | 'ignore' | null | undefined;
  interface Readable {
    setEncoding(encoding: string): this;
    on(event: 'data', listener: (chunk: Buffer | string) => void): this;
    on(event: 'end' | 'close', listener: () => void): this;
  }
  interface Writable {
    write(chunk: string | Uint8Array): boolean;
    end(chunk?: string | Uint8Array): void;
  }
  interface ChildProcess {
    readonly pid: number | undefined;
    readonly stdin: Writable | null;
    readonly stdout: Readable | null;
    readonly stderr: Readable | null;
    readonly exitCode: number | null;
    readonly signalCode: string | null;
    readonly killed: boolean;
    kill(signal?: string | number): boolean;
    on(event: 'exit' | 'close', listener: (code: number | null, signal: string | null) => void): this;
    on(event: 'error', listener: (error: Error) => void): this;
    on(event: 'spawn', listener: () => void): this;
    once(event: 'exit' | 'close', listener: (code: number | null, signal: string | null) => void): this;
    once(event: 'error', listener: (error: Error) => void): this;
    once(event: 'spawn', listener: () => void): this;
  }
  interface CommonOptions {
    cwd?: string;
    env?: Record<string, string | undefined>;
    timeout?: number;
    killSignal?: string | number;
  }
  interface SpawnOptions extends CommonOptions {
    shell?: boolean | string;
    stdio?: StdioOption | StdioOption[];
  }
  interface ExecOptions extends CommonOptions {
    shell?: string;
    maxBuffer?: number;
    encoding?: string | null;
  }
  interface SyncOptions extends SpawnOptions {
    input?: string | Uint8Array;
    maxBuffer?: number;
    encoding?: string | null;
  }
  interface SpawnSyncReturns<T> {
    pid: number;
    output: Array<T | null>;
    stdout: T;
    stderr: T;
    status: number | null;
    signal: string | null;
    error?: Error;
  }
  interface ExecException extends Error {
    code?: number | string;
    signal?: string;
    cmd?: string;
  }
  type ExecCallback = (error: ExecException | null, stdout: string, stderr: string) => void;
  export function spawn(command: string, args?: readonly string[], options?: SpawnOptions): ChildProcess;
  export function spawnSync(
    command: string,
    args?: readonly string[],
    options?: SyncOptions & { encoding: string },
  ): SpawnSyncReturns<string>;
  export function spawnSync(command: string, args?: readonly string[], options?: SyncOptions): SpawnSyncReturns<Buffer>;
  export function exec(command: string, callback?: ExecCallback): ChildProcess;
  export function exec(command: string, options: ExecOptions, callback?: ExecCallback): ChildProcess;
  export namespace exec {
    function __promisify__(command: string, options?: ExecOptions): Promise<{ stdout: string; stderr: string }>;
  }
  export function execFile(file: string, callback?: ExecCallback): ChildProcess;
  export function execFile(file: string, args: readonly string[], callback?: ExecCallback): ChildProcess;
  export function execFile(
    file: string,
    args: readonly string[],
    options: ExecOptions,
    callback?: ExecCallback,
  ): ChildProcess;
  export namespace execFile {
    function __promisify__(
      file: string,
      args?: readonly string[],
      options?: ExecOptions,
    ): Promise<{ stdout: string; stderr: string }>;
  }
  export function execSync(command: string, options: SyncOptions & { encoding: string }): string;
  export function execSync(command: string, options?: SyncOptions): Buffer;
  export function execFileSync(file: string, args: readonly string[], options: SyncOptions & { encoding: string }): string;
  export function execFileSync(file: string, args?: readonly string[], options?: SyncOptions): Buffer;
}

declare module 'node:util' {
  type Callback<T> = (error: unknown, result: T) => void;
  /** The promised form a function names through `util.promisify.custom`. */
  export function promisify<F extends { __promisify__: (...args: never[]) => unknown }>(fn: F): F['__promisify__'];
  export function promisify<T>(fn: (callback: Callback<T>) => void): () => Promise<T>;
  export function promisify<A, T>(fn: (a: A, callback: Callback<T>) => void): (a: A) => Promise<T>;
  export function promisify<A, B, T>(fn: (a: A, b: B, callback: Callback<T>) => void): (a: A, b: B) => Promise<T>;
  export namespace promisify {
    const custom: unique symbol;
  }
  export function callbackify<T>(fn: () => Promise<T>): (callback: Callback<T>) => void;
  export function callbackify<A, T>(fn: (a: A) => Promise<T>): (a: A, callback: Callback<T>) => void;
  export function deprecate<F extends (...args: never[]) => unknown>(fn: F, message: string, code?: string): F;
  export function inherits(constructor: unknown, superConstructor: unknown): void;
  export function format(format?: unknown, ...args: unknown[]): string;
  export function inspect(value: unknown, options?: { depth?: number | null; colors?: boolean }): string;
  export function isDeepStrictEqual(a: unknown, b: unknown): boolean;
}
