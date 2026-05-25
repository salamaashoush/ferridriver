---
pageType: home

hero:
  name: ferridriver
  text: Rust-native browser automation.
  tagline: Playwright-compatible API. Four backends behind one surface. Test runner, BDD with native JS/TS step bodies, and an MCP server — one binary.
  actions:
    - theme: brand
      text: Get started
      link: /guide/quickstart
    - theme: alt
      text: Why ferridriver
      link: /comparison/
    - theme: alt
      text: GitHub
      link: https://github.com/salamaashoush/ferridriver

features:
  - title: Rust engine, four backends
    icon: 🦀
    details: One Browser/Page/Locator surface dispatching to CDP over pipes (default), CDP over WebSocket, Playwright WebKit, and Firefox over WebDriver BiDi. Enum dispatch — no vtable cost.
    link: /concepts/backends
  - title: Browser API for Node and Bun
    icon: 📦
    details: "@ferridriver/node ships the same Browser/Page/Locator/Frame/BrowserContext to Node.js and Bun via NAPI-RS. The Rust engine does the work; the JS layer is a thin typed wrapper."
    link: /api/rust
  - title: Test runner included
    icon: ⚡
    details: "#[ferritest] in Rust. Parallel workers, DAG-resolved fixtures, hooks, retries with flaky detection, 38 auto-retrying expect matchers, snapshots, Playwright-compatible traces."
    link: /test-runner/overview
  - title: BDD with native JS/TS steps
    icon: 🥒
    details: Gherkin backed by the Page API. Step bodies in Rust or JavaScript/TypeScript — TS/JS files bundle to QuickJS bytecode once and run through the same runner. No Node or Bun in the loop.
    link: /bdd/overview
  - title: MCP server in the box
    icon: 🤖
    details: Scripting-focused MCP server with 10 tools. run_script runs sandboxed JavaScript against the live session with full Page / Context / HttpClient bindings. Multi-step automation in one LLM turn.
    link: /mcp/overview
  - title: One binary
    icon: 📥
    details: "`ferridriver` is the only thing to install. MCP server, BDD runner, script runner, test wrapper, browser installer — all subcommands."
    link: /cli/ferridriver
  - title: Recipes
    icon: 📚
    details: Copy-paste patterns for login + saved auth state, network mocking, file upload/download, multi-tab, mobile emulation, screenshots, traces, and CI on GitHub Actions.
    link: /recipes/overview
  - title: Honest about gaps
    icon: 📝
    details: We do not ship position-stability checks or codegen yet. The comparison page lists every gap against Playwright, Puppeteer, Selenium, and Cypress.
    link: /comparison/
  - title: Troubleshoot fast
    icon: 🔧
    details: Common install issues, flake patterns, MCP setup quirks, CI fixes, and an FAQ. Hit a problem in 5 minutes, find the fix in 30 seconds.
    link: /troubleshooting/
---
