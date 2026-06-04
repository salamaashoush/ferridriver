# Scripting overview

ferridriver embeds a QuickJS engine (`ferridriver-script`) and exposes
it to four first-class surfaces. All four share one bundler
(rolldown), one bytecode cache, one set of globals, and one sandbox.

| Surface              | Entry point                          | What you write |
|----------------------|--------------------------------------|----------------|
| **MCP `run_script`** | tool call from an LLM client          | sandboxed JavaScript against the live browser session |
| **BDD JS / TS steps**| `ferridriver bdd --steps 'steps/**/*.{js,ts}'` | `Given` / `When` / `Then` step bodies for `.feature` files |
| **Extensions**       | `extensions = ["@scope/pkg", "./extensions"]` in `ferridriver.toml` | ESM packages or source files that register MCP tools (`tool`) and / or BDD steps |
| **Standalone scripts** | `ferridriver run script.ts`        | Playwright-style scripts with `chromium()` / `firefox()` / `webkit()` globals |

All four reach the same Rust core (`Page`, `BrowserContext`, `Locator`,
`HttpClient`). The JS layer is a thin binding — the hot path
(actionability checks, polling, network) stays in Rust.

## Pipeline (compile once, run many)

```
source files (.js / .ts / .mjs / .tsx / ...)
        │
        ▼  rolldown (TypeScript + node_modules + tree-shake, Platform::Neutral, OutputFormat::Esm)
        │
        ▼  QuickJS bytecode (in-memory, content-hash cached)
        │
        ▼  Module::load per session VM (no re-parse, no resolver — imports already inlined)
        │
        ▼  top-level Given() / tool() side effects populate the Rust ExtensionRegistry
```

- **Imports work.** `import './helpers.ts'`, `import pkg from 'some-dep'`,
  and `import { tool, bdd } from 'ferridriver'` are bundled and
  tree-shaken. Cucumber-compatible code can import from
  `@cucumber/cucumber`.
- **No Node, no Bun in the run path.** Rolldown runs `Platform::Neutral`;
  QuickJS has no Node builtins.
- **Bytecode is cached in-memory and on disk.** An in-process cache
  serves repeat compiles within one process; a cross-process disk cache
  (under the user cache dir, or `FERRIDRIVER_CACHE_DIR`) lets an
  unchanged source tree skip both rolldown and the QuickJS compile on a
  fresh start. Disk entries live under an ABI-tag directory (QuickJS
  version, arch, endianness, pointer width) so `Module::load` only ever
  reads bytecode from a matching toolchain; a mismatch misses and
  recompiles. Set `FERRIDRIVER_NO_BYTECODE_CACHE` to disable the disk
  cache.
- **One bad file does not abort the batch.** Bundle / compile failures
  are reported per file and skipped; the server still starts.
- **Errors are source-mapped.** A thrown error in a bundled step is
  reported at the original `.ts:line:col`, stack included.

## Picking a surface

- **MCP `run_script`** — agent-driven flows. One tool call runs many
  browser operations in one LLM turn. See [`/mcp/tools`](/mcp/tools)
  and the [run_script reference](/scripting/run-script).
- **BDD JS / TS steps** — human-readable `.feature` files with step
  bodies your TS team already knows how to write. See
  [`/scripting/bdd-js-api`](/scripting/bdd-js-api) and
  [`/bdd/overview`](/bdd/overview).
- **Extensions** — when one `.ts` file should contribute *both* a
  reusable MCP tool *and* matching BDD step. The same file can ship
  both. See [`/scripting/extensions`](/scripting/extensions).
- **Standalone scripts** — one-off automation runs from the CLI
  (`ferridriver run`).

## Pages in this section

- [Extensions](/scripting/extensions) — `tool`, `exposeAsMcpTool`,
  `ferridriver.host`, handler context, lifecycle.
- [BDD JS / TS API](/scripting/bdd-js-api) — full Cucumber-shaped
  reference: `Given` / `When` / `Then` / `defineStep`, hooks, World,
  `DataTable`, parameter types.
- [`run_script` reference](/scripting/run-script) — the MCP action
  path: parameters, return shape, globals.
- [Sandbox](/scripting/sandbox) — `process`, `fetch`, `fs`,
  `AbortController`, `ReadableStream`, `Blob`, `FormData`, what is
  absent.
- [Capabilities](/scripting/capabilities) — `allow.commands` (declared
  shell / argv commands) and `allow.net` (HTTP host allow-list).
- [State and sessions](/scripting/state-and-sessions) — `globalThis`
  vs `vars`, session lifetime, isolation, BDD per-scenario World.

Design rationale (why the system is shaped this way — VS Code / Deno /
WASM / Rollup comparison) is in the maintainer note
[`docs/plugin-architecture.md`](https://github.com/salamaashoush/ferridriver/blob/main/docs/plugin-architecture.md).
