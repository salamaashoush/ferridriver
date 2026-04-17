# Introduction

ferridriver is a high-performance browser automation library written in Rust, with a Playwright-compatible API. It ships as:

- A Rust library ([`ferridriver`](https://crates.io/crates/ferridriver))
- Node.js / Bun bindings ([`@ferridriver/node`](https://www.npmjs.com/package/@ferridriver/node))
- A cross-language test runner ([`ferridriver-test`](https://crates.io/crates/ferridriver-test) + [`@ferridriver/test`](https://www.npmjs.com/package/@ferridriver/test))
- A BDD / Gherkin framework ([`ferridriver-bdd`](https://crates.io/crates/ferridriver-bdd))
- An MCP server for AI agents ([`ferridriver-cli`](https://crates.io/crates/ferridriver-cli))
- Component-testing adapters for React, Vue, Svelte, and Solid

## Why ferridriver

- **Four backends, one API.** CDP over pipes (fastest), CDP over WebSocket, native WKWebView on macOS, and WebDriver BiDi for Firefox.
- **Built on the same core.** The test runner, BDD framework, MCP server, and NAPI bindings all dispatch to one engine — no duplicated logic.
- **Performance.** Overlapped browser launches, MPMC work-stealing dispatch, and a Rust-native CDP client.
- **First-class test runner.** Auto-retrying assertions, DAG-resolved fixtures, hooks, text and pixel-diff snapshots, video, traces, and JUnit / HTML / JSON reporters.

## Next steps

- [Quickstart](/guide/quickstart) — your first script in Rust or TypeScript
- [Installation](/guide/installation) — CLI, library, and browser setup
- [Architecture](/guide/architecture) — how the backends, test runner, and MCP server fit together
