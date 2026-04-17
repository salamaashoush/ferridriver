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
  - title: Also great from Node and Bun
    details: NAPI bindings expose the same API with TypeScript types. Mix Rust and JS test suites in one repo; they share the same engine.
    link: /api/typescript
  - title: Four backends, one API
    details: CDP over pipes, CDP over WebSocket, native WKWebView on macOS, and Firefox via WebDriver BiDi. Switch with a single flag.
    link: /concepts/backends
  - title: Test runner included
    details: Parallel workers, DAG-resolved fixtures, hooks, retries, auto-retrying expect matchers, snapshot, trace, and Playwright-compatible reporters.
    link: /test-runner/overview
  - title: Component testing for four frameworks
    details: Mount React, Vue, Svelte, or Solid components in a real browser and drive them with the full Page/Locator API.
    link: /component-testing/overview
  - title: BDD and MCP in the box
    details: 144 built-in Gherkin steps backed by the Page API. Scripting-focused MCP server with 9 tools and full Page/Context/APIRequest bindings via `run_script`. Mix .feature + .spec.ts + MCP in one run.
    link: /bdd/overview
---
