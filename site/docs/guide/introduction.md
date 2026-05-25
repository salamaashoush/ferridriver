# Introduction

ferridriver is browser automation written in Rust with a Playwright-shaped
API. A single core crate (`ferridriver`) implements `Browser`,
`BrowserContext`, `Page`, `Frame`, `Locator`, `ElementHandle`. Everything
above it — the test runner, the BDD framework, the MCP server, the NAPI
binding — is a thin translator over that core. There is no second
implementation, no JSON-RPC sidecar.

## What ships

| Component                | Crate / package |
|--------------------------|-----------------|
| Rust library             | [`ferridriver`](https://crates.io/crates/ferridriver) |
| Test runner              | [`ferridriver-test`](https://crates.io/crates/ferridriver-test) |
| Expect matchers          | [`ferridriver-expect`](https://crates.io/crates/ferridriver-expect) |
| BDD framework            | [`ferridriver-bdd`](https://crates.io/crates/ferridriver-bdd) |
| QuickJS engine           | [`ferridriver-script`](https://crates.io/crates/ferridriver-script) |
| MCP server library       | [`ferridriver-mcp`](https://crates.io/crates/ferridriver-mcp) |
| CLI binary               | [`ferridriver-cli`](https://crates.io/crates/ferridriver-cli) |
| Node / Bun browser API   | [`@ferridriver/node`](https://www.npmjs.com/package/@ferridriver/node) |

## Why ferridriver

**If you're on Rust.** Your app is Rust, your CI is Rust, your team
writes Rust. Running Playwright means standing up a Node toolchain you do
not otherwise need. ferridriver is the same Playwright-shaped API in the
language you ship in. Tests live next to the code they cover.

**If you're on JS.** `@ferridriver/node` is a thin typed browser API.
The Rust engine handles the hot path (selector compilation, auto-wait
polling, CDP transport), so the NAPI surface is low-overhead. Drive a
browser directly, or write Gherkin features whose step bodies are
JavaScript / TypeScript and run them with `ferridriver bdd`.

**If you're both.** Write Rust `#[ferritest]` tests and Gherkin features
whose step bodies are Rust or TypeScript — same engine, same backend,
same reporters, driven by the single `ferridriver` binary.

## What's in the box

- **One engine, multiple frontends.** Test runner, BDD framework, MCP
  server, and NAPI binding all dispatch to the same `Browser::launch`
  and `Page::click`.
- **Four backends behind one API.** CDP over pipes (default), CDP over
  WebSocket, Playwright WebKit (cross-platform), and Firefox over
  WebDriver BiDi.
- **Auto-waiting.** Actionability checks before every action; Playwright
  polling schedule (`100, 250, 500, 1000` ms) on every assertion.
- **Parallel test execution.** One browser per worker, fresh context per
  test, MPMC work-stealing dispatch.
- **Everything you expect.** Fixtures, hooks, retries with flaky
  detection, text and pixel snapshots, video, Playwright-compatible
  traces, terminal / HTML / JSON / JUnit / Cucumber Messages reporters.

## Where to go next

- [Installation](/guide/installation) — install the CLI and a browser.
- [Quickstart](/guide/quickstart) — a running example in Rust or TypeScript.
- [Architecture](/guide/architecture) — how the engine, runner, and backends fit together.
- [Migrating from Playwright](/concepts/migrating-from-playwright) — honest delta, including current gaps.
