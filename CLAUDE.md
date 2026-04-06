# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**ferridriver** is a high-performance browser automation library in Rust with a Playwright-compatible API. It supports multiple CDP backends and native WebKit, exposes an MCP server for AI agents, and provides Node.js/Bun bindings via NAPI-RS.

## Build Commands

Uses `just` (justfile) and cargo aliases (`.cargo/config.toml`):

| Command | Purpose |
|---|---|
| `just check` (or `just c`) | Type-check workspace |
| `just test` | Build binary + run all workspace tests + CLI backend tests |
| `just test-backend cdp_pipe` | Run tests for a single backend (`cdp_pipe`, `cdp_raw`, `webkit`) |
| `just test-ts` | NAPI/TypeScript tests with Bun |
| `just bdd *args` | Run BDD feature tests |
| `just lint` | `cargo clippy --workspace --all-targets -- -D warnings` |
| `just fmt` | Format check |
| `just fix` (or `just f`) | Auto-fix lint + format |
| `just ready` (or `just r`) | Full CI gate: fmt + lint + test |
| `just build` | Release build (full LTO, strip) |
| `just build-fast` | Release-fast (thin LTO, parallel codegen) |
| `just run` | Run MCP server (stdio) |
| `just run-http` | Run MCP server (HTTP, port 8080) |

Cargo aliases: `cargo ck`, `cargo lint`, `cargo lintfix`, `cargo release`, `cargo release-fast`.

## Workspace Structure

12 crates in `crates/`:

```
ferridriver              Core library: Browser, Page, Locator, Frame, backends
ferridriver-mcp          MCP server library (rmcp-based, stdio + HTTP transports)
ferridriver-cli          CLI binary (`ferridriver mcp/test/bdd`)
ferridriver-napi         Node.js/Bun native addon via NAPI-RS (thin target over core)
ferridriver-test         E2E test runner: parallel workers, fixtures, reporters, retries
ferridriver-test-macros  Proc macros: #[ferritest], #[ferritest_each]
ferridriver-bdd          BDD/Cucumber framework: step registry, Gherkin parser, translators
ferridriver-bdd-macros   Proc macros: #[given], #[when], #[then], #[step]
ferridriver-ct-leptos    Component testing adapter for Leptos (trunk build + serve)
ferridriver-ct-leptos-macros  Proc macro: #[component_test] for Leptos
ferridriver-ct-dioxus    Component testing adapter for Dioxus (dx build + serve)
ferridriver-ct-dioxus-macros  Proc macro: #[component_test] for Dioxus
```

TS packages in `packages/`:

```
packages/ferridriver-test    TS CLI + test API (test.each, describe, expect, BDD steps)
```

Dependency flow: `ferridriver-cli` -> `ferridriver-mcp` -> `ferridriver` <- `ferridriver-napi`

Test framework flow: `ferridriver-cli` -> `ferridriver-bdd` -> `ferridriver-test` -> `ferridriver`

## Architecture

### Core Principle

Rust is the source of truth. NAPI is a thin target. TS is a thin wrapper.

- All filtering (grep, only, skip, fixme, shard, last-failed) happens in the core runner
- All expect/assertion polling happens in Rust via NAPI expect methods
- The NAPI test runner delegates to `TestRunner::run()` — no separate execution loop
- `TestAnnotation` is shared between Rust and TS via serde serialization
- Never duplicate logic in NAPI/TS that exists in Rust core

### Backend System (enum dispatch, not trait objects)

Three backends in `crates/ferridriver/src/backend/`:

- **CdpPipe** (default) — CDP over Unix pipes (fd 3/4), lowest latency, launches Chrome
- **CdpRaw** — CDP over WebSocket, fully parallel, can connect to running Chrome
- **WebKit** — macOS-only native WKWebView via Objective-C subprocess IPC

Backend directory structure:
```
backend/
  cdp/
    mod.rs          Unified CDP implementation (~87KB)
    pipe.rs         Pipe transport
    ws.rs           WebSocket transport
    transport.rs    Transport abstraction + CDP tracing
  webkit/
    mod.rs
    ipc.rs
```

### Test Runner (`ferridriver-test`)

- `TestRunner::run()` is the single execution pipeline for ALL test types (E2E, BDD, NAPI, CT)
- Workers launch browsers, create pages, inject fixtures, run hooks, handle retries
- `TestAnnotation` enum: Skip, Slow, Fixme (with condition), Fail, Only, Tag, Info
- Conditional fixme evaluates platform/browser/CI at runtime before test body runs
- `filter_by_only`, `filter_by_grep`, `filter_by_rerun`, `check_forbid_only` in discovery.rs
- Centralized logging via `ferridriver_test::logging::init()` — respects RUST_LOG, FERRIDRIVER_DEBUG, --verbose

### MCP Server (`ferridriver-mcp`)

- `McpServer` in `server.rs` holds shared `Arc<Mutex<BrowserState>>`
- Tools are organized by category in `tools/` (navigation, input, content, cookies, storage, emulation, network, bdd)
- `McpServerConfig` trait allows customization (chrome args, auth, metadata)

### Key Source Files

- `crates/ferridriver/src/page.rs` — Page API (~60KB)
- `crates/ferridriver/src/locator.rs` — Locator API (~36KB)
- `crates/ferridriver/src/backend/cdp/mod.rs` — Unified CDP backend (~87KB)
- `crates/ferridriver-mcp/src/server.rs` — MCP server core (~27KB)
- `crates/ferridriver-test/src/runner.rs` — Test runner orchestrator
- `crates/ferridriver-test/src/worker.rs` — Worker: browser, fixtures, hooks, retries
- `crates/ferridriver-test/src/expect/` — Auto-retrying assertions (Playwright-style errors)
- `crates/ferridriver-test/src/logging.rs` — Centralized tracing init

## Code Style & Linting

- **Nightly Rust** toolchain, edition 2024
- **2-space indentation**, 120 char line width (see `rustfmt.toml`)
- Clippy: `correctness`/`perf`/`suspicious` = **deny**, `style`/`complexity`/`pedantic` = warn
- `unwrap_used`, `expect_used`, `todo`, `dbg_macro` = warn (relaxed in tests via `clippy.toml`)
- `unsafe_code` = warn
- Uses `FxHashMap` (rustc-hash) instead of `std::HashMap` for performance
- `avoid-breaking-exported-api = false` — breaking API changes are acceptable

## Testing

~430 total tests: ~94 Rust tests + ~337 NAPI/TS tests (Bun) + 83 BDD scenarios (81 pass, 2 skip).

Tests require a Chrome/Chromium binary. The CLI backend tests use `FERRIDRIVER_BIN` env var pointing to the built binary (handled automatically by `just test`).

To verify BDD baseline: `./target/debug/ferridriver bdd -j 2 -- tests/features/*.feature` — expect 83 scenarios: 81 passed, 2 skipped.

To build NAPI .node binary: `cd crates/ferridriver-napi && bun run build:debug`

## Git Commits

- Never add `Co-Authored-By`, `Generated by`, or any AI/Claude/Anthropic attribution to commit messages
- Commit messages should look like they were written by the developer

## Benchmarks

In `bench/` directory, Bun-based. See `bench/CLAUDE.md` for Bun conventions.
