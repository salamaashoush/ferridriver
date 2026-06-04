# ferridriver-script

[![crates.io](https://img.shields.io/crates/v/ferridriver-script.svg?logo=rust&color=c97b4a)](https://crates.io/crates/ferridriver-script)
[![docs.rs](https://img.shields.io/docsrs/ferridriver-script?logo=docs.rs&color=c97b4a)](https://docs.rs/ferridriver-script)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-c97b4a.svg)](https://github.com/salamaashoush/ferridriver)

QuickJS engine for ferridriver. Powers three surfaces with one runtime:

- **`run_script`** (MCP tool) — sandboxed JavaScript with live browser bindings.
- **BDD JS / TS step bodies** — `ferridriver bdd --steps 'steps/**/*.{js,ts}'`.
- **JS / TS extension files** — `tool` (MCP) and `Given` / `When` / `Then` (BDD) in one module.

There is no Node or Bun in the run path. Sources are bundled with
rolldown, compiled to QuickJS bytecode once, and loaded into per-session
VMs via `Module::load`.

## Pipeline

```
source files (.js/.ts/.mjs/.tsx/...)
        │
        ▼  rolldown (TypeScript + node_modules + tree-shake, Platform::Neutral, OutputFormat::Esm)
        │
        ▼  QuickJS bytecode (in-memory, content-hash cached)
        │
        ▼  Module::load per session VM (no re-parse, no resolver — imports already inlined)
        │
        ▼  top-level Given() / tool() side effects populate the Rust ExtensionRegistry
```

The bundler runs once per file change. Bytecode is content-hashed and
kept in memory only — `Module::load` bytecode is interpreter-build- and
process-specific, so persisting to disk would violate that invariant.
Errors are remapped to original source line:col via the rolldown source
map.

## Public API (programmatic use)

```rust
use ferridriver_script::{bundle::bundle_and_compile, engine::ScriptEngine};

// One-shot bundle + compile to bytecode.
let bundle = bundle_and_compile(&entry_paths, &cwd).await?;

// Load into a per-session VM and run a script.
let mut engine = ScriptEngine::new(config).await?;
engine.eval_bundle(&bundle).await?;
let result = engine.run("await page.title()").await?;
```

## Sandbox

- `process.env` defaults to `{}`. Names listed in `[scripting] allowEnv`
  and present in the server's environment appear, frozen. `process.exit`,
  `process.binding`, `process.dlopen`, `process.kill`, `process.chdir`,
  `process.setuid`, … are absent.
- `fs` is scoped to `script_root`. Absolute paths, `..`, and symlink
  escapes are rejected.
- `fetch` shares the same HTTP core as `request`. Any `allow.net`
  restriction on a tool binds both.
- `commands.run` is gated by the tool's declared `allow.commands` map
  (default-deny). Templates are trusted; values are escaped.

See [`docs/extensions.md`](../../docs/extensions.md) for the full
authoring contract.

## License

MIT OR Apache-2.0
