---
pageType: home

hero:
  name: ferridriver
  text: Fast browser automation in Rust.
  tagline: Playwright-compatible API. Four backends. Built-in test runner, BDD, component testing, and MCP server for AI agents.
  actions:
    - theme: brand
      text: Get started
      link: /guide/quickstart
    - theme: alt
      text: View on GitHub
      link: https://github.com/salamaashoush/ferridriver

features:
  - title: Playwright-compatible
    details: Familiar Page, Locator, Frame, and BrowserContext APIs in idiomatic Rust, plus TypeScript bindings for Node.js and Bun.
    link: /guide/introduction
  - title: Four backends
    details: CDP over pipes (fastest), CDP over WebSocket, native WebKit on macOS, and Firefox via WebDriver BiDi.
    link: /guide/architecture
  - title: Test runner included
    details: Parallel workers, fixtures, hooks, retries, auto-retrying expect matchers, snapshot and trace support.
    link: /test-runner/overview
  - title: Component testing
    details: First-class adapters for React, Vue, Svelte, and Solid. Mount components in a real browser using the same Page API.
    link: /component-testing/overview
  - title: BDD out of the box
    details: 144 built-in Gherkin steps backed by the Page/Locator API. Mix .feature files with .spec.ts in one run.
    link: /bdd/overview
  - title: MCP server for AI agents
    details: 28 browser automation tools over stdio or HTTP. Works with Claude, Cursor, Claude Code, and any MCP client.
    link: /mcp/overview
---
