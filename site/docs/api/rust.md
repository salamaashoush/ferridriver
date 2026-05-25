# Rust API reference

Full rustdoc for every crate is published on [docs.rs](https://docs.rs/):

- [`ferridriver`](https://docs.rs/ferridriver) — core `Browser`, `BrowserContext`, `Page`, `Frame`, `Locator`, `ElementHandle`, `Route`
- [`ferridriver-test`](https://docs.rs/ferridriver-test) — `TestRunner`, `#[ferritest]`, fixtures, hooks, reporters
- [`ferridriver-expect`](https://docs.rs/ferridriver-expect) — auto-retrying `expect` matchers
- [`ferridriver-bdd`](https://docs.rs/ferridriver-bdd) — `BrowserWorld`, step macros, registry, `bdd_main!()`, executor
- [`ferridriver-mcp`](https://docs.rs/ferridriver-mcp) — `McpServer`, `McpServerConfig`, tool registry
- [`ferridriver-config`](https://docs.rs/ferridriver-config) — `FerridriverConfig`, `TestConfig`, `McpConfig`, `BrowserConfig`, `ProjectConfig`, `WebServerConfig`, …
- [`ferridriver-script`](https://docs.rs/ferridriver-script) — QuickJS engine
- [`ferridriver-cli`](https://docs.rs/ferridriver-cli) — CLI binary crate
- [`ferridriver-test-macros`](https://docs.rs/ferridriver-test-macros) — `#[ferritest]`, `#[ferritest_each]`, hook macros
- [`ferridriver-bdd-macros`](https://docs.rs/ferridriver-bdd-macros) — `#[given]`, `#[when]`, `#[then]`, `#[step]`, `#[before]`, `#[after]`, `#[param_type]`

`ferridriver-node` is a `cdylib` NAPI addon and is not published on
crates.io. The TypeScript surface lives in
[`@ferridriver/node`](https://www.npmjs.com/package/@ferridriver/node).
