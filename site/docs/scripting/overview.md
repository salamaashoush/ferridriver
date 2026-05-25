# Scripting overview

ferridriver embeds a QuickJS engine (`ferridriver-script`) and exposes
it to three first-class surfaces. All three share one bundler
(rolldown), one bytecode cache, one set of globals, and one sandbox.

| Surface              | Entry point                          | What you write |
|----------------------|--------------------------------------|----------------|
| **MCP `run_script`** | tool call from an LLM client          | sandboxed JavaScript against the live browser session |
| **BDD JS / TS steps**| `ferridriver bdd --steps 'steps/**/*.{js,ts}'` | `Given` / `When` / `Then` step bodies for `.feature` files |
| **Extensions**       | `extensions = ["..."]` in `ferridriver.toml` | `.ts` / `.js` files that register MCP tools (`defineTool`) and / or BDD steps |
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
        ▼  top-level Given() / defineTool() side effects populate the Rust ExtensionRegistry
```

- **Imports work.** `import './helpers.ts'`, `import pkg from 'some-dep'`
  — all bundled and tree-shaken.
- **No Node, no Bun in the run path.** Rolldown runs `Platform::Neutral`;
  QuickJS has no Node builtins.
- **Bytecode cache is in-memory and process-local.** It is never written
  to disk — a requirement of the `Module::load` invariant (bytecode is
  interpreter-build- and process-specific).
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

- [Extensions](/scripting/extensions) — `defineTool`, `exposeAsTool`,
  `ferridriver.host`, handler context, lifecycle.
- [BDD JS / TS API](/scripting/bdd-js-api) — full Cucumber-shaped
  reference: `Given` / `When` / `Then` / `Step`, hooks, World,
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
