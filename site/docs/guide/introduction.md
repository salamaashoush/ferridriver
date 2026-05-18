# Introduction

ferridriver is browser automation written in Rust, with a Playwright-compatible API. If you're a Rust team building a web app, you don't have to context-switch to Node to write your end-to-end tests. If you're already on JS, the NAPI binding gives you the same engine with TypeScript types.

It ships as:

- A **Rust library** — [`ferridriver`](https://crates.io/crates/ferridriver)
- A **browser API for Node.js / Bun** — [`@ferridriver/node`](https://www.npmjs.com/package/@ferridriver/node) (`Browser`/`Page`/`Locator`/`Frame`/`BrowserContext`)
- A **test runner** (Rust) — [`ferridriver-test`](https://crates.io/crates/ferridriver-test)
- A **BDD framework** — [`ferridriver-bdd`](https://crates.io/crates/ferridriver-bdd), with Rust or JavaScript/TypeScript step bodies
- An **MCP server** for AI agents — [`ferridriver-cli`](https://crates.io/crates/ferridriver-cli)

## Why ferridriver

**If you're on Rust.** Your app is Rust. Your CI is Rust. Your team writes Rust. Running Playwright means standing up a Node toolchain you don't otherwise need, on a test pyramid your product code doesn't share. ferridriver is the same Playwright-shaped API, in the language you already ship in. Tests live next to the code they cover.

**If you're on JS.** `@ferridriver/node` is a thin, typed browser API — `Browser`/`Page`/`Locator`/`Frame`/`BrowserContext`. The Rust engine does the actual work (auto-wait polling, selector compilation, CDP transport), so the NAPI surface is low-overhead. Drive a browser directly, or write Gherkin steps in JS/TS and run them with `ferridriver bdd`.

**If you're both.** Write Rust `#[ferritest]` tests and Gherkin features whose step bodies are Rust or JavaScript/TypeScript — all on the same engine, backend, and reporter, driven by the single `ferridriver` binary.

## What's in the box

- **One engine, multiple frontends.** The test runner, BDD framework, MCP server, and NAPI layer all dispatch to the same core. No forked implementations.
- **Four backends behind one API.** CDP over pipes (default), CDP over WebSocket, native WKWebView on macOS, and Firefox via WebDriver BiDi.
- **Auto-waiting.** Actionability checks before every action; Playwright-cadence polling (`100, 250, 500, 1000 ms`) on every assertion.
- **Parallel test execution.** One browser per worker, fresh context per test. MPMC work-stealing dispatch.
- **Everything you expect in a test runner.** Fixtures, hooks, retries with flaky detection, text and pixel-diff snapshots, video, CDP traces (Playwright-compatible), JUnit / HTML / JSON reporters.

## Where to go next

- [Quickstart](/guide/quickstart) — a running example in five lines (Rust or TypeScript).
- [Architecture](/guide/architecture) — how the engine, runner, and backends fit together.
- [Migrating from Playwright](/concepts/migrating-from-playwright) — the honest delta, including what's still missing.
