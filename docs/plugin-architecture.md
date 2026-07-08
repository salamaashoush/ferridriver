# Plugin architecture survey

Context: the ferridriver MCP extension system loads JS/TS files whose
top-level `defineTool(...)` calls register into the Rust extension
registry, and exposes each tool as a `tools.<name>(args)` binding
(optionally promoted to an MCP tool). This
note surveys mature plugin/extension systems for three concerns —
compile-once-run-many, capability/permission models, API extensibility —
and records what ferridriver adopts versus defers, with the tradeoff.

## Comparison

| System | Compile-once | Capability model | Extensibility shape | Takeaway for us |
|---|---|---|---|---|
| **VS Code extension host** | Lazy: extensions activate on declared *activation events*, code parsed on first need | Coarse (separate process, no fs/net sandbox); trust = install | *Contribution points*: extensions declare static JSON (commands, menus) + register dynamic handlers | Static manifest declaration + lazy activation is the durable idea. Process isolation is overkill for our in-VM model. |
| **esbuild / Rollup / Vite** | Plugins are config, bundler compiles graph once | None (build-time, trusted) | **Ordered hook pipeline** with `enforce: 'pre' \| 'post'`, named hooks (`resolveId`/`load`/`transform`) | Ordered, named, phase-tagged hooks are the gold standard *when there is cross-cutting work to order*. We have none yet. |
| **Playwright fixtures (`test.extend`)** | n/a (per-worker) | n/a | Dependency-injected, **lazily instantiated**, scoped (test/worker), override by name | Lazy + DI + named override is exactly our `{ args, page, context, request }` injection. Validates the current handler signature. |
| **oclif / Backstage** | Manifest (`package.json` `oclif`/ catalog) drives discovery; code loaded on demand | Trust-based; Backstage adds permission framework (policy decisions per action) | Manifest-declared commands/plugins; Backstage = central policy engine | Declarative manifest discovery + an *external* allow/deny policy maps cleanly onto our `allow.*`. |
| **tower / axum middleware** | Compiled Rust, zero runtime cost | Type system | `Layer` stack: ordered, composable, wraps inner service | The right model *if* we need request-wrapping middleware. We do not (no auth/logging cross-cut today). |
| **WASM Component Model / WASI** | Module compiled once, instantiated many | **Capability-based**: no ambient authority; host hands in typed resource handles (a preopened dir, a socket) | Typed interfaces (WIT); imports are the only authority | The capability principle — *no ambient authority, host grants explicit scoped handles* — is the security model to copy. Full WASM isolation is a much larger rewrite than warranted. |
| **Deno / Bun** | Bundle/compile cached by content | **Declarative permission flags** (`--allow-net=host`, `--allow-read=path`, `--allow-run=cmd`) granted at process start, enforced at syscall | Bun: native plugin API (`onResolve`/`onLoad`); Deno: import-map + permissions | Deno's *scoped, declarative, default-deny net/fs/exec grants* is the directly-applicable capability model. Content-hash compile cache is the directly-applicable perf idea. |

## Decisions

**Adopt now**

1. **rolldown→QuickJS-bytecode, compile-once** (Deno/WASM): plugins join the
   exact pipeline BDD steps use — bundle (TS + plugin-local imports + tree
   shake) once, compile to bytecode once, `Module::load` per session (no
   parse). Tradeoff: rolldown bundle cost moves to startup (one-time, ~ms
   per file) in exchange for zero per-session parse and free TS/imports.
2. **Declarative capability manifest** (Deno `--allow-*` + WASM no-ambient-
   authority): `allow` becomes a named, default-deny capability set,
   Rust-owned and enforced at the binding boundary. Shipped capabilities:
   **exec** (`allow.commands`, `exec` accepted as a synonym — fully enforced
   already) and **net** (`allow.net`: host allow-list on the handler's
   `request` client; empty = unrestricted for back-compat, non-empty flips
   that binding to default-deny). `fs` is deliberately **not** a capability:
   the plugin handler context (`{args,page,context,request,commands}`)
   exposes no filesystem handle, so an `fs` scope would gate nothing — a
   stub, which the repo rules forbid. `net` is scoped precisely to the
   `request` HTTP client and documented as such; `page`/`context` browser
   navigation is a separate, deliberately ungated authority (an automation
   plugin must navigate), so this is a *complete* boundary for what it
   covers, not a partial one giving false confidence. Tradeoff: a slightly
   larger manifest for an auditable, honestly-scoped sandbox.
3. **Content-hash bytecode cache** (Deno): key compiled bytecode by source
   content hash; identical/unchanged files skip rebundle+recompile. In-memory
   only — `Module::load` bytecode is interpreter-build- and process-specific,
   so a persisted disk cache would violate that invariant.

**Defer, with rationale** (CLAUDE.md: do not build speculative abstractions
without a consumer)

- **Ordered hook/middleware pipeline** (Rollup/tower): high-value *only* when
  there is cross-cutting work to order. box-craft's 20 tools have none; adding
  a pipeline now is a hypothetical-future abstraction. Revisit when a real
  cross-cut (auth, tracing) appears — the capability boundary already gives a
  natural insertion point.
- **Plugin-registered reusable bindings** (sharing helpers across files):
  with rolldown, files import shared helpers directly (`import './util.ts'`),
  which covers the real need without new API surface. Cross-*plugin* shared
  state has no consumer.
- **Plugin dependency/ordering** (`dependsOn`): the loader already sorts files
  deterministically by path and files are scope-isolated; no inter-file order
  dependency exists in the shipped bundle.
- **Lifecycle hooks** (`onLoad`/`onActivate`/`onSessionStart`): module
  top-level code already *is* `onLoad` for free under ESM. A distinct
  per-session `onActivate` has no current consumer and adds parity surface
  across the two JS layers; defer until a plugin needs per-session setup that
  module-eval cannot express.

Net: adopt the two ideas with immediate, concrete value (compile-once
pipeline parity; complete declarative sandbox) and the one perf primitive
(content-hash cache); decline the four speculative ones until a consumer
exists, keeping the JS surface a thin mirror of Rust per the parity rules.
