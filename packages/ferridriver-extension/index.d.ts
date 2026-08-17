// Type declarations for ferridriver extensions.
//
// An extension is a `.ts`/`.js` module that calls `defineTool(...)` at the
// top level. The registration functions and the handler's capabilities are
// native Rust functions injected into the QuickJS context, so there is
// nothing to import at run time — this package carries only the
// editor/typecheck surface, and `ferridriver ext check` uses it to
// type-check an extension against the binary that will load it.
//
// Browser types (`Page`, `BrowserContext`, `Locator`, ...) come from
// `@ferridriver/test`: a handler drives the exact same bindings a test
// does, so they must not drift into a second hand-written copy.
//
// There are intentionally no index-signature escapes: a missing
// declaration is a visible type error, not a silently-any call.

import type {
  APIRequestContext,
  Browser,
  BrowserContext,
  BrowserType,
  Page,
} from '@ferridriver/test';

export type {
  APIRequestContext,
  Browser,
  BrowserContext,
  BrowserType,
  ElementHandle,
  Frame,
  FrameLocator,
  Locator,
  Page,
  Request,
  Response,
  Route,
} from '@ferridriver/test';

// ── Handler context ──────────────────────────────────────────────────

/** Which browser session (and therefore which environment) the handler drives. */
export interface SessionRef {
  /** The full session key, `"<instance>:<context>"`. */
  key: string;
  /**
   * The instance half: which browser process. Configured under
   * `[mcp.browser.instances.<name>]`, so this is what tells a handler
   * which environment it is pointed at — derive an env from this rather
   * than taking one as an argument.
   */
  instance: string;
  /** The context half: which cookie/storage-isolated browser context. */
  context: string;
}

/** Session-scoped string store. Survives VM rebuilds for the session's life. */
export interface Vars {
  get(name: string): string | undefined;
  set(name: string, value: string): void;
  has(name: string): boolean;
  delete(name: string): void;
  keys(): string[];
}

/** Sandboxed filesystem, confined to the configured `scriptRoot`. */
export interface Fs {
  /** Absolute path of the sandbox root. */
  readonly root: string;
  readFile(path: string): Promise<string>;
  readFileBytes(path: string): Promise<number[]>;
  readFileSync(path: string): string;
  readFileBytesSync(path: string): number[];
  writeFile(path: string, contents: string): Promise<void>;
  readdir(path: string): Promise<string[]>;
  exists(path: string): Promise<boolean>;
  existsSync(path: string): boolean;
}

/** Output sandbox (`artifactsRoot`) for screenshots, PDFs, traces, downloads. */
export interface Artifacts {
  /** Absolute path of the artifacts root. */
  readonly root: string;
  write(name: string, contents: string): Promise<void>;
  writeBytes(name: string, bytes: Uint8Array | number[]): Promise<void>;
  read(name: string): Promise<string>;
  readBytes(name: string): Promise<number[]>;
  list(): Promise<string[]>;
  readdir(subpath: string): Promise<string[]>;
  exists(name: string): Promise<boolean>;
  remove(name: string): Promise<boolean>;
}

/**
 * A connected sidecar process, driven over fd 3/4 with NUL-delimited JSON.
 * Long-lived: the connection belongs to the session, so repeated calls do
 * not pay a process spawn.
 */
export interface Sidecar {
  readonly name: string;
  send<T = unknown>(method: string, params?: unknown): Promise<T>;
  /** Pipeline many calls in one round trip; results are positional. */
  sendMany<T = unknown>(calls: { method: string; params?: unknown }[]): Promise<T[]>;
  on(event: string, listener: (payload: unknown) => void): (payload: unknown) => void;
  once<T = unknown>(event: string): Promise<T>;
  off(event: string, listener?: (payload: unknown) => void): void;
  close(): Promise<void>;
}

/** Connect to a sidecar declared in `[[sidecars]]`. Declared names only. */
export interface Sidecars {
  connect(name: string): Promise<Sidecar>;
}

/**
 * Runner for the commands the tool declared in `allow.commands`.
 * Default-deny: a name the manifest does not declare throws before exec.
 *
 * `vars` fill the `${name}` placeholders in the declared template; every
 * placeholder must be supplied.
 */
export interface Commands {
  run<T = unknown>(name: string, vars?: Record<string, string | number | boolean>): Promise<T>;
  /** Start a persistent process (idempotent while it is running). */
  start(name: string, vars?: Record<string, string | number | boolean>): Promise<{ name: string; pid: number }>;
  status(name: string): Promise<{
    name: string;
    running: boolean;
    pid?: number;
    exitCode?: number | null;
    stdout?: string;
    stderr?: string;
  }>;
  stop(name: string): Promise<void>;
}

export type LogLevel = 'error' | 'warn' | 'info' | 'debug' | 'trace';

/**
 * Diagnostics attributed to the tool, routed through the host's `tracing`
 * subscriber — so `-v`, `FERRIDRIVER_DEBUG` and `RUST_LOG` control it the
 * same way they control the engine's own output. `log(msg)` is `info`.
 */
export interface Log {
  (message: string, fields?: Record<string, unknown>): void;
  error(message: string, fields?: Record<string, unknown>): void;
  warn(message: string, fields?: Record<string, unknown>): void;
  info(message: string, fields?: Record<string, unknown>): void;
  debug(message: string, fields?: Record<string, unknown>): void;
  trace(message: string, fields?: Record<string, unknown>): void;
  /** Whether the operator's filter would record this level. */
  enabled(level: LogLevel): boolean;
}

/**
 * What a tool handler receives.
 *
 * `Settings` is the shape of this tool's `[extensions.settings.<key>]`
 * block; declare a JSON Schema for it under `ferridriver.settings` in the
 * package's `package.json` and it is validated before the extension loads.
 */
export interface ToolContext<Args = Record<string, unknown>, Settings = Record<string, unknown>> {
  args: Args;
  page: Page;
  context: BrowserContext;
  request: APIRequestContext;
  browser: Browser;
  commands: Commands;
  vars: Vars;
  fs: Fs;
  artifacts: Artifacts;
  sidecars: Sidecars;
  settings: Settings;
  session: SessionRef | undefined;
  log: Log;
  /** Fires when the tool's `timeoutMs` expires. Pass it to `fetch`/listeners. */
  signal: AbortSignal;
}

// ── Registration ─────────────────────────────────────────────────────

/**
 * One named command a tool may run: a shell string (`sh -c`, so `$(…)`,
 * pipes and redirection work), an argv array (no shell), or the full
 * object form. `${name}` placeholders are filled from `commands.run`'s
 * `vars` — strictly: an unsupplied placeholder is an error, never an
 * empty string.
 */
export type CommandSpec =
  | string
  | {
      run: string | string[];
      /** Per-command wall-clock bound in milliseconds. */
      timeoutMs?: number;
      /** Environment variable names to pass through (default: none). */
      env?: string[];
      cwd?: string;
      /** How stdout is shaped: raw text, parsed JSON, or split lines. */
      output?: 'text' | 'json' | 'lines';
      /** Managed by `commands.start`/`status`/`stop` instead of `run`. */
      persistent?: boolean;
    };

/**
 * What a tool NEEDS. The operator's `[extensions.policy]` is what it is
 * GRANTED; the effective authority is the intersection.
 */
export interface ToolAllow {
  /** Named command templates. Default-deny: undeclared names cannot run. */
  commands?: Record<string, CommandSpec>;
  /** Alias of `commands`. */
  exec?: Record<string, CommandSpec>;
  /**
   * Hosts the handler's `request`/`fetch` may target: exact
   * (`api.acme.com`) or leading-wildcard (`*.acme.com`, which also matches
   * the apex). Empty leaves HTTP unrestricted; a non-empty list flips it
   * to default-deny.
   */
  net?: string[];
}

/** MCP tool annotations, passed through to `tools/list`. Hints only. */
export interface ToolAnnotations {
  title?: string;
  readOnlyHint?: boolean;
  destructiveHint?: boolean;
  idempotentHint?: boolean;
  openWorldHint?: boolean;
}

export interface ToolDefinition<
  Args = Record<string, unknown>,
  Result = unknown,
  Settings = Record<string, unknown>,
> {
  /**
   * Globally unique name. Also the binding key (`tools['acme.login']`) and,
   * when promoted, the MCP tool name. Dot-separated namespacing is the
   * convention; the part before the first dot is the settings namespace.
   */
  name: string;
  /** Human label for `tools/list` (MCP separates it from `name`). */
  title?: string;
  description?: string;
  /** JSON Schema for `args`. Enforced before the handler runs. */
  inputSchema?: object;
  /** JSON Schema for the return value. Enforced, and shipped as `structuredContent`. */
  outputSchema?: object;
  annotations?: ToolAnnotations;
  allow?: ToolAllow;
  /** Register as a first-class MCP tool (not just a `tools.<name>` binding). */
  exposeAsTool?: boolean;
  /** Alias of `exposeAsTool`. */
  exposeAsMcpTool?: boolean;
  /**
   * Per-invocation bound in milliseconds. Cooperative: the race can only
   * win while the handler is awaiting, and `ctx.signal` fires when it does.
   */
  timeoutMs?: number;
  handler: (ctx: ToolContext<Args, Settings>) => Promise<Result> | Result;
}

/** Which host is running this module. */
export type ExtensionHost = 'mcp' | 'bdd' | 'test' | 'script';

// ── package.json `ferridriver` manifest ──────────────────────────────

/** Host preconditions a package declares. Declarations, not grants. */
export interface ExtensionRequires {
  /** Programs that must be on `PATH`. */
  commands?: string[];
  /** Names the operator must list in `[scripting].allowEnv`. */
  env?: string[];
  /** Hosts that must fit inside the `[extensions.policy]` net ceiling. */
  net?: string[];
  /** Names some `[[sidecars]]` entry must declare. */
  sidecars?: string[];
}

/**
 * The `ferridriver` field of an extension package's `package.json`.
 *
 * ```json
 * { "ferridriver": { "entries": ["./src/login.ts"] } }
 * ```
 */
export interface ExtensionPackageManifest {
  /**
   * Modules to load as extensions, in declaration order; paths relative
   * to the package directory (a file, extension optional, or a directory
   * scanned recursively). Anything not listed is reachable only as an
   * import of an entry — which is what keeps a shared `lib/` from being
   * loaded as a tool-less extension.
   */
  entries?: string[];
  requires?: ExtensionRequires;
  /**
   * JSON Schema per `[extensions.settings.<key>]` block the package
   * reads, keyed by tool namespace or full tool name. Validated against
   * the operator's config when the extension loads.
   */
  settings?: Record<string, object>;
}

// ── Globals ──────────────────────────────────────────────────────────

declare global {
  /**
   * Register a tool. Call at the module's top level: registration is a
   * side effect of the module running, there is no `activate()` hook.
   */
  function defineTool<
    Args = Record<string, unknown>,
    Result = unknown,
    Settings = Record<string, unknown>,
  >(definition: ToolDefinition<Args, Result, Settings>): void;

  /** Alias of `defineTool`. */
  function tool<
    Args = Record<string, unknown>,
    Result = unknown,
    Settings = Record<string, unknown>,
  >(definition: ToolDefinition<Args, Result, Settings>): void;

  /** Call another registered tool, including from a different extension. */
  const tools: Record<string, (args?: Record<string, unknown>) => Promise<unknown>>;

  const ferridriver: {
    /** Branch on this so one module can serve the MCP, BDD and script hosts. */
    readonly host: ExtensionHost;
    readonly commands: Commands;
  };

  /**
   * Only the names the operator allow-listed in `[scripting].allowEnv`,
   * and only those actually set — an unlisted name is `undefined`, never
   * invented.
   */
  const process: { env: Record<string, string | undefined> };

  // Outside a handler (a `ferridriver run` script, or a BDD step file
  // bundled from an extension) the same capabilities are globals.
  const vars: Vars;
  const fs: Fs;
  const artifacts: Artifacts;
  const sidecars: Sidecars;
  const commands: Commands;
  const request: APIRequestContext;

  // Browser bindings under the `script` host. Declared with `var` because a
  // script (or an extension bootstrapping one, e.g. a compatibility shim for
  // another tool's script format) may assign them via `globalThis`, which
  // only type-checks for `var` globals.
  var page: Page;
  var context: BrowserContext;
  var browser: Browser;

  /** Launch a browser of your own, independent of the host's. */
  function chromium(options?: { transport?: 'pipe' | 'ws' }): BrowserType;
  function firefox(): BrowserType;
  function webkit(): BrowserType;
}
