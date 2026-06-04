# ferridriver-mcp

[![crates.io](https://img.shields.io/crates/v/ferridriver-mcp.svg?logo=rust&color=c97b4a)](https://crates.io/crates/ferridriver-mcp)
[![docs.rs](https://img.shields.io/docsrs/ferridriver-mcp?logo=docs.rs&color=c97b4a)](https://docs.rs/ferridriver-mcp)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-c97b4a.svg)](https://github.com/salamaashoush/ferridriver)

MCP (Model Context Protocol) server library for ferridriver. Implements
ten tools focused on scripted browser automation; `run_script` is the
primary action path, the other tools are cheap observation and bootstrap.

This crate is the **library** behind `ferridriver mcp`. The binary that
ships it is `ferridriver-cli`. Depend on `ferridriver-mcp` directly only
when embedding the server in another Rust process.

## Tools

| Tool                    | Category    | Purpose |
|-------------------------|-------------|---------|
| `connect`               | Bootstrap   | Attach to a running Chrome (debugger URL or auto-discovery) |
| `navigate`              | Bootstrap   | Go to a URL; returns a fresh accessibility snapshot |
| `page`                  | Bootstrap   | Manage pages: `back`, `forward`, `reload`, `new`, `close`, `select`, `list`, `close_browser` |
| `snapshot`              | Observation | Accessibility tree with `[ref=eN]` handles. Primary grounding tool |
| `screenshot`            | Observation | PNG / JPEG / WebP base64. Heavier than `snapshot` — use sparingly |
| `evaluate`              | Observation | Single JavaScript expression in page; returns JSON value |
| `search_page`           | Observation | Grep page text (literal or regex) with context |
| `diagnostics`           | Observation | Console messages, network requests, performance metrics |
| `run_script`            | Action      | Sandboxed QuickJS runtime with `page` / `context` / `request` / `browser` |
| `ferridriver_extensions`| Introspection | List loaded extension files and their tools |

All tools accept an optional `session` parameter (default `"default"`).
Format: `instance:context` — `instance` selects the browser process,
`context` selects the BrowserContext within it. Sessions have isolated
cookies, localStorage, and network state.

## Transports

- **stdio** (default) — Claude Desktop, Cursor, Claude Code.
- **HTTP** — listens on `0.0.0.0:<port>/mcp` with stateful sessions.

No authentication is built in. Deploy behind a firewall / reverse proxy.

## `run_script` bindings

| Global     | Notes |
|------------|-------|
| `page`     | Playwright-shaped Page API over ferridriver core |
| `context`  | BrowserContext — cookies, permissions, init scripts, headers, geolocation |
| `request`  | `HttpClient` for runner-side HTTP (`get`, `post`, `put`, `delete`, `patch`, `head`, `fetch`) |
| `browser`  | Browser handle for multi-page operations |
| `args`     | Positional arguments. Bound, never interpolated into the source — prompt-injection safe |
| `vars`     | Session-scoped `get` / `set` / `has` / `delete` / `keys`. Persists across `run_script` calls in the same session |
| `console`  | Captured `log` / `info` / `warn` / `error` / `debug` (1000 entries / 1 MiB / 8 KiB per entry, ANSI-stripped) |
| `fs`       | Scoped to `script_root`: `readFile`, `writeFile`, `readdir`, `exists`. Absolute paths, `..`, and symlink escapes are rejected |
| `artifacts`| Dedicated output dir for screenshots, PDFs, traces |
| `fetch`, `Headers`, `Request`, `Response`, `AbortController`, `Blob`, `FormData`, `ReadableStream` | Standard web APIs |
| `process`  | Sandbox-safe subset: `platform`, `arch`, `version`, `versions`, `argv`, `pid`, `cwd`, `stdout`, `stderr`. `process.env` is `{}` by default; opt-in keys via `[scripting] allowEnv` |

ES module `import './foo.js'` resolves inside `script_root` with the same
sandbox rules. Bare specifiers (`import 'lodash'`) are rejected.

## Embedding

```rust
use ferridriver_mcp::{serve_stdio, serve_http};
use ferridriver::backend::BackendKind;
use ferridriver::state::ConnectMode;

// stdio transport (mode, backend, headless)
serve_stdio(ConnectMode::Launch, BackendKind::CdpPipe, /* headless */ true).await?;

// or HTTP (mode, backend, port, headless) — serves on 0.0.0.0:<port>/mcp
serve_http(ConnectMode::Launch, BackendKind::CdpPipe, 8080, /* headless */ true).await?;
```

To customize the server (config, extensions, extra tools), build an
`McpServer` and serve it via `serve_stdio_with(server)` /
`serve_http_with(server, port)`.

Implement `McpServerConfig` to override `script_root`, `artifacts_root`,
script engine config, base Chrome args, per-instance args, instance
resolution, server name, and instructions.

## Configuration

In `ferridriver.toml`:

```toml
[mcp.server]
name = "ferridriver"
# extra_instructions = "..."

[mcp.browser]
backend = "cdp-pipe"
headless = false
# executable_path = "/path/to/chrome"
chrome_args = ["--no-default-browser-check"]
command_cache_ttl = 300

[mcp.browser.viewport]
width = 1280
height = 720

[mcp.browser.instances.staging]
chrome_args = ["--proxy-server=staging-proxy:8080"]
connect_url = "ws://staging-host:9222"
```

## Extensions

JavaScript / TypeScript extension files declare additional MCP tools via
`tool` and / or BDD steps via `Given` / `When` / `Then`. They run on
the same QuickJS engine as `run_script` and BDD step bodies. See
[`docs/extensions.md`](../../docs/extensions.md) for the contract.

## License

MIT OR Apache-2.0
