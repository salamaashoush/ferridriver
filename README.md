# ferridriver

[![CI](https://github.com/salamaashoush/ferridriver/actions/workflows/ci.yml/badge.svg)](https://github.com/salamaashoush/ferridriver/actions/workflows/ci.yml)
[![Docs](https://github.com/salamaashoush/ferridriver/actions/workflows/docs.yml/badge.svg)](https://salamaashoush.github.io/ferridriver/)
[![crates.io](https://img.shields.io/crates/v/ferridriver.svg?logo=rust&color=c97b4a)](https://crates.io/crates/ferridriver)
[![docs.rs](https://img.shields.io/docsrs/ferridriver?logo=docs.rs&color=c97b4a)](https://docs.rs/ferridriver)
[![npm](https://img.shields.io/npm/v/@ferridriver/node.svg?logo=npm&color=c97b4a)](https://www.npmjs.com/package/@ferridriver/node)
[![MSRV](https://img.shields.io/badge/MSRV-1.91-c97b4a?logo=rust)](./rust-toolchain.toml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-c97b4a.svg)](./README.md#license)

Browser automation written in Rust with a Playwright-compatible API. Four
backends (Chromium over CDP pipes, Chromium over CDP WebSocket, Playwright
WebKit, Firefox over WebDriver BiDi) behind one surface. Ships as:

- A Rust library — `ferridriver`
- A test runner — `ferridriver-test` with `#[ferritest]`, fixtures, hooks, expect matchers
- A BDD framework — `ferridriver-bdd` with native Gherkin and step bodies in Rust or JavaScript/TypeScript
- A core browser binding for Node.js / Bun — `@ferridriver/node` (NAPI-RS)
- A CLI binary — `ferridriver` (MCP server, BDD runner, script runner, test wrapper, browser installer)

JavaScript / TypeScript BDD step files run **natively** through the single
Rust binary: they are bundled with rolldown, compiled to QuickJS bytecode
once at startup, and executed on the embedded `ferridriver-script` engine.
**No Node or Bun is involved in the run path.**

Status: pre-1.0. The API tracks Playwright closely but is not API-stable —
expect breaking changes between minor versions.

## Project layout

11 workspace crates plus one example crate.

| Crate                          | Purpose |
|--------------------------------|---------|
| `ferridriver`                  | Core: `Browser`, `BrowserContext`, `Page`, `Frame`, `Locator`, `ElementHandle`, network routing, selectors, backends |
| `ferridriver-config`           | Unified config schema (`ferridriver.{toml,yaml,json}` — `[mcp]`, `[test]`, `[scripting]`, `[extensions]`) |
| `ferridriver-mcp`              | MCP server library (rmcp-based; stdio + HTTP transports; 10 tools) |
| `ferridriver-cli`              | CLI binary: `mcp`, `bdd`, `test`, `run`, `install` subcommands |
| `ferridriver-script`           | QuickJS engine — backs `run_script`, JS/TS BDD steps, and JS extensions |
| `ferridriver-node`             | NAPI-RS binding shipping the browser API to Node.js / Bun |
| `ferridriver-test`             | Test runner core — parallel workers, fixtures, hooks, retries, reporters, snapshots, traces |
| `ferridriver-test-macros`      | `#[ferritest]`, `#[ferritest_each]`, `#[fixture]`, `#[ferritest_suite]`, hook attribute macros |
| `ferridriver-expect`           | Auto-retrying assertion library — locator, page, value, polling matchers |
| `ferridriver-bdd`              | BDD framework — Gherkin parser, step / hook registry, scenario translator, executor |
| `ferridriver-bdd-macros`       | `#[given]`, `#[when]`, `#[then]`, `#[step]`, `#[before]`, `#[after]`, `#[param_type]` |
| `examples/bdd-example`         | Reference Rust BDD suite (feature files + Rust step bodies) |

Everything above the core is a thin translator. The same `Page::click`
implementation is reached by a Rust `#[ferritest]`, a Gherkin `When I click
"..."` step, a JavaScript line in a `run_script` MCP call, and a Node.js
`page.locator(...).click()` over NAPI.

## Install

### One-line install (Linux, macOS)

```bash
curl -fsSL https://raw.githubusercontent.com/salamaashoush/ferridriver/main/install.sh | bash
```

Installs system dependencies (Linux), the `ferridriver` binary, and
Chromium for Testing.

### Manual install

```bash
# From crates.io
cargo install ferridriver-cli

# From GitHub releases (prebuilt binaries)
curl -fsSL https://github.com/salamaashoush/ferridriver/releases/latest/download/ferridriver-VERSION-TARGET.tar.gz | tar xz
```

### Browsers

```bash
ferridriver install chromium                          # default
ferridriver install --with-deps chromium              # Linux: also install system libraries
ferridriver install firefox chromium-headless-shell   # multiple at once
```

The WebKit backend uses Playwright's WebKit binary. `ferridriver install
webkit` downloads it into ferridriver's own cache. Alternatively, set
`FERRIDRIVER_WEBKIT` to a Playwright WebKit checkout containing
`pw_run.sh`, or install Playwright once (`npx playwright install webkit`)
and ferridriver picks up that cache.

### Node.js / Bun (core browser binding only)

```bash
npm install @ferridriver/node
# or
bun add @ferridriver/node
```

Platform binaries are pulled in via `optionalDependencies`
(`@ferridriver/node-{darwin-arm64,linux-x64-gnu,linux-arm64-gnu}`).

## Quick start (Rust)

```rust
use ferridriver::{Browser, browser_type::chromium};
use ferridriver::options::LaunchOptions;
use ferridriver::url_matcher::UrlMatcher;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let browser = chromium().launch(LaunchOptions::default()).await?;
    let page = browser.page().await?;

    page.goto("https://example.com", None).await?;
    page.locator("#email", None).fill("test@example.com", None).await?;
    page.locator("button[type=submit]", None).click(None).await?;
    page.wait_for_url(UrlMatcher::glob("**/dashboard")?).await?;

    let png = page.screenshot(Default::default()).await?;
    std::fs::write("home.png", png)?;

    browser.close().await?;
    Ok(())
}
```

`firefox()` and `webkit()` are factories with the same shape. `chromium()`
defaults to the `CdpPipe` backend; use `BrowserType::chromium_with(&BrowserTypeOptions
{ transport: Some(ChromiumTransport::Ws), .. })` for `CdpRaw`.

## Quick start (Node.js / Bun)

```ts
import { Browser } from '@ferridriver/node';

const browser = await Browser.launch();
const page = await browser.newPageWithUrl('https://example.com');

await page.locator('#email').fill('test@example.com');
await page.locator('button[type=submit]').click();
await page.waitForUrl('/dashboard');

await browser.close();
```

## Tests

Three first-class authoring styles. All run on the same `TestRunner` —
same workers, same retries, same reporters.

### 1. Rust `#[ferritest]`

```rust
use ferridriver_test::prelude::*;

#[ferritest]
async fn loads_homepage(ctx: TestContext) {
    let page = ctx.page().await?;
    page.goto("https://example.com", None).await?;
    expect(&page).to_have_title("Example Domain").await?;
}

#[ferritest(retries = 2, tag = "smoke", timeout = "30s")]
async fn login_flow(ctx: TestContext) {
    let page = ctx.page().await?;
    page.goto("https://app.example.com/login", None).await?;
    page.locator("#email", None).fill("user@example.com", None).await?;
    page.locator("button[type=submit]", None).click(None).await?;
    expect(&page).to_have_url("/dashboard").await?;
}
```

Wire a binary entry point:

```rust
// tests/harness.rs
mod homepage;
mod login;
ferridriver_test::main!();
```

```toml
# Cargo.toml
[[test]]
name = "e2e"
path = "tests/harness.rs"
harness = false

[dev-dependencies]
ferridriver-test = "0.3"
```

```bash
cargo test --test e2e
cargo test --test e2e -- --headless --backend webkit -j 1
cargo test --test e2e -- -g smoke --retries 2
```

### 2. BDD with Rust step bodies

```rust
use ferridriver_bdd::prelude::*;

#[given("I navigate to {string}")]
async fn navigate(world: &mut BrowserWorld, url: String) {
    world.page().goto(&url, None).await.unwrap();
}

#[when("I click {string}")]
async fn click(world: &mut BrowserWorld, selector: String) {
    world.page().locator(&selector, None).click(None).await.unwrap();
}
```

```rust
// tests/bdd.rs
ferridriver_bdd::bdd_main!();
```

```bash
cargo test --test bdd
# or via the CLI:
ferridriver bdd tests/features/
```

`ferridriver-bdd` ships 145 built-in steps (assertions, interaction,
navigation, network, API, storage, keyboard, mouse, frame, dialog,
emulation, etc.) — write your own only for app-specific vocabulary.

### 3. BDD with JavaScript / TypeScript step bodies

```ts
// steps/login.ts
Given('I navigate to {string}', async function (url: string) {
  await this.page.goto(url);
});

When('I click {string}', async function (selector: string) {
  await this.page.locator(selector).click();
});

Then('the URL should contain {string}', async function (fragment: string) {
  if (!this.page.url().includes(fragment)) {
    throw new Error(`URL ${this.page.url()} does not contain ${fragment}`);
  }
});
```

```bash
ferridriver bdd --steps 'steps/**/*.{js,ts}' tests/features/
```

`Given` / `When` / `Then` / `defineStep` / `Before` / `After` /
`defineParameterType` / `setWorldConstructor` / `setDefaultTimeout` are
global; `this` is the `World` with `page` / `context` / `request` /
`browser` / `parameters` / `attach` / `log` / `skip`. No `package.json`,
no `node_modules`.

## MCP server

Scripting-focused MCP server for AI agent browser automation. Works with
Claude Code, Claude Desktop, Cursor, or any MCP client.

```bash
# stdio transport (Claude Code, Cursor)
ferridriver mcp

# HTTP transport (remote clients)
ferridriver mcp --transport http --port 8080

# Backend choice + headless
ferridriver mcp --backend webkit --headless

# Attach to an already-running Chrome
ferridriver mcp --auto-connect chrome
ferridriver mcp --connect ws://localhost:9222/devtools/browser/...
```

**10 tools.** `connect`, `navigate`, `page` (session bootstrap) · `snapshot`,
`screenshot`, `evaluate`, `search_page`, `diagnostics` (observation) ·
`run_script` (action) · `ferridriver_extensions` (introspection).

`run_script` runs sandboxed JavaScript against the live session with full
`Page` / `Locator` / `BrowserContext` / `HttpClient` bindings. One script
can navigate, fill forms, click, assert, and make HTTP calls in a single
LLM turn:

```js
await page.goto(args[0]);
await page.getByLabel('Email').fill(args[1]);
await page.getByLabel('Password').fill(args[2]);
await page.getByRole('button', { name: 'Sign in' }).click();
await page.waitForSelector('[data-testid="dashboard"]');
return { title: await page.title(), cookies: await context.cookies() };
```

Globals in a script: `page`, `context`, `request`, `browser`, `args`
(bound, not interpolated — prompt-injection safe), `vars` (session-level
key/value store), `console`, `fs` (scoped to `script_root`), `artifacts`
(dedicated output dir), plus standard `fetch` / `Headers` / `Request` /
`Response` / `AbortController`. Error responses include stack, line,
column, and a source snippet so the model can self-correct.

See [site/docs/mcp/tools.md](./site/docs/mcp/tools.md) and
[docs/extensions.md](./docs/extensions.md) for the full surface and the
plugin/extension contract.

## Backends

| Backend     | Browser            | Transport                                    | Default? |
|-------------|--------------------|----------------------------------------------|----------|
| `cdp-pipe`  | Chromium / Chrome  | CDP over Unix pipes (fd 3/4)                 | yes      |
| `cdp-raw`   | Chromium / Chrome  | CDP over WebSocket (can attach via `connect`) |          |
| `webkit`    | Playwright WebKit  | Playwright Inspector protocol over `pw_run.sh` |        |
| `bidi`      | Firefox            | WebDriver BiDi over WebSocket                |          |

Backends dispatch through a Rust `enum`, not a trait object — monomorphic
calls, no vtable lookup.

WebKit speaks Playwright's WebKit Inspector protocol over a NUL-byte-
delimited JSON pipe to a `pw_run.sh` child process. Same code on every
platform (macOS, Linux, Windows). The binary is shipped by Playwright;
ferridriver locates it via `FERRIDRIVER_WEBKIT`, then the Playwright
cache, then the ferridriver cache. Run `ferridriver install webkit`
(or `npx playwright install webkit`, or set `FERRIDRIVER_WEBKIT`) to
provide it.

## Build and test

The repository uses `just` (`justfile`) and cargo aliases (`.cargo/config.toml`).

| Command | Purpose |
|---------|---------|
| `just check` (alias `just c`) | `cargo check --all-targets` |
| `just test`  (alias `just t`) | Build the binary, run every Rust crate's tests (all 4 backends), then the BDD feature suite |
| `just test-fast` (alias `just tf`) | Same as `test` but with maximum parallelism (one backend per shell) |
| `just test-backend cdp_pipe` | Run a single backend's integration tests (`cdp_pipe`, `cdp_raw`, `webkit`, `bidi`) |
| `just bdd <args>`            | Run BDD features against `tests/features/` |
| `just lint`                  | `cargo clippy --workspace --all-targets -- -D warnings` |
| `just fmt`                   | `cargo fmt --all -- --check` |
| `just fix` (alias `just f`)  | Format then auto-fix lints |
| `just ready` (alias `just r`) | Full CI gate: fmt + lint + test |
| `just build`                 | Release build (full LTO, strip) |
| `just build-fast`            | Release-fast profile (thin LTO, parallel codegen) |
| `just run <args>`            | Run the binary directly |
| `just run-http [port]`       | MCP server on HTTP transport |
| `just release X.Y.Z`         | Bump version, commit, tag, push (triggers release CI) |

The Node binding lives outside the workspace default-members. To build and
test it locally:

```bash
cd crates/ferridriver-node
bun install
bun run build:debug
bun test
```

## Requirements

- Rust nightly (edition 2024). The toolchain is pinned in
  `rust-toolchain.toml`; `rust-version` (MSRV) is 1.91.
- Chrome or Chromium (`ferridriver install chromium`, or set
  `FERRIDRIVER_BROWSERS_PATH` to use an existing install).
- Firefox installed locally for the `bidi` backend (ferridriver does not
  bundle Firefox).
- Playwright WebKit binary for the `webkit` backend (`ferridriver install
  webkit`, set `FERRIDRIVER_WEBKIT`, or use Playwright's cache).
- `ffmpeg` on `PATH` at runtime for video recording (optional).
- Node.js 16+ or Bun 1.0+ only if you build or consume `@ferridriver/node`.

## Documentation

- Full site: <https://salamaashoush.github.io/ferridriver/>
- Per-crate rustdoc: <https://docs.rs/ferridriver>
- Architecture and internals: [`CLAUDE.md`](./CLAUDE.md), [`docs/`](./docs/)

## License

MIT OR Apache-2.0
