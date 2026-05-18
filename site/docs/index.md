---
pageType: home

hero:
  name: ferridriver
  text: Browser automation for Rust projects.
  tagline: Playwright-compatible API, native Rust engine. Don't switch to Node to write end-to-end tests — but use Node or Bun if you want to, the bindings are first-class.
  actions:
    - theme: brand
      text: Get started
      link: /guide/quickstart
    - theme: alt
      text: View on GitHub
      link: https://github.com/salamaashoush/ferridriver

features:
  - title: Rust-native engine
    details: Browser, Page, Locator, Frame, BrowserContext — the types you already know from Playwright, written in idiomatic Rust. No Node sidecar, no JSON-RPC to shell out to.
    link: /guide/introduction
  - title: Browser API for Node and Bun
    details: "@ferridriver/node exposes Browser/Page/Locator/Frame/BrowserContext with TypeScript types, backed by the same Rust engine."
    link: /api/rust
  - title: Four backends, one API
    details: CDP over pipes, CDP over WebSocket, native WKWebView on macOS, and Firefox via WebDriver BiDi. Switch with a single flag.
    link: /concepts/backends
  - title: Test runner included
    details: Parallel workers, DAG-resolved fixtures, hooks, retries, auto-retrying expect matchers, snapshot, trace, and Playwright-compatible reporters.
    link: /test-runner/overview
  - title: BDD with native JS/TS steps
    details: Gherkin steps backed by the Page API. Step bodies in Rust or JavaScript/TypeScript — TS/JS files bundle to QuickJS bytecode and run through the core runner with no Node or Bun in the loop.
    link: /bdd/overview
  - title: MCP server in the box
    details: Scripting-focused MCP server with 9 tools and full Page/Context/APIRequest bindings via `run_script`. Mix .feature scenarios + MCP in one run.
    link: /mcp/overview
---
