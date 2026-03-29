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

Four crates in `crates/`:

```
ferridriver          Core library: Browser, Page, Locator, Frame, backends, BDD steps
ferridriver-mcp      MCP server library (25 tools, rmcp-based, stdio + HTTP transports)
ferridriver-cli      CLI binary (`ferridriver mcp`) — thin wrapper around ferridriver-mcp
ferridriver-napi     Node.js/Bun native addon via NAPI-RS
```

Dependency flow: `ferridriver-cli` -> `ferridriver-mcp` -> `ferridriver` <- `ferridriver-napi`

## Architecture

### Backend System (enum dispatch, not trait objects)

Three backends in `crates/ferridriver/src/backend/`:

- **cdp_pipe** (default) — CDP over Unix pipes (fd 3/4), lowest latency, launches Chrome
- **cdp_raw** — CDP over WebSocket, fully parallel, can connect to running Chrome
- **webkit** — macOS-only native WKWebView via Objective-C subprocess IPC

`AnyBrowser` and `AnyPage` enums dispatch to concrete backend types at zero cost. New backends require adding variants to these enums in `backend/mod.rs`.

### MCP Server (`ferridriver-mcp`)

- `McpServer` in `server.rs` holds shared `Arc<Mutex<BrowserState>>`
- Tools are organized by category in `tools/` (navigation, input, content, cookies, storage, emulation, network, bdd)
- `McpServerConfig` trait allows customization (chrome args, auth, metadata)
- Routers composed via `McpServer::combined_router()`

### Key Source Files

- `crates/ferridriver/src/page.rs` — Page API (~60KB, largest file)
- `crates/ferridriver/src/locator.rs` — Locator API (~32KB)
- `crates/ferridriver/src/backend/cdp_pipe/mod.rs` — CDP pipe backend (~94KB)
- `crates/ferridriver/src/backend/cdp_raw/mod.rs` — CDP raw backend (~90KB)
- `crates/ferridriver-mcp/src/server.rs` — MCP server core (~100KB)

## Code Style & Linting

- **Nightly Rust** toolchain, edition 2024
- **2-space indentation**, 120 char line width (see `rustfmt.toml`)
- Clippy: `correctness`/`perf`/`suspicious` = **deny**, `style`/`complexity`/`pedantic` = warn
- `unwrap_used`, `expect_used`, `todo`, `dbg_macro` = warn (relaxed in tests via `clippy.toml`)
- `unsafe_code` = warn
- Uses `FxHashMap` (rustc-hash) instead of `std::HashMap` for performance
- `avoid-breaking-exported-api = false` — breaking API changes are acceptable

## Testing

317 total tests: 67 Rust integration tests (53 BDD + 14 Page API) + 250 NAPI tests (Bun).

Tests require a Chrome/Chromium binary. The CLI backend tests use `FERRIDRIVER_BIN` env var pointing to the built binary (handled automatically by `just test`).

## Benchmarks

In `bench/` directory, Bun-based. See `bench/CLAUDE.md` for Bun conventions.
