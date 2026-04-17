# Rust API reference

Full rustdoc for every crate is published on [docs.rs](https://docs.rs/):

- [`ferridriver`](https://docs.rs/ferridriver) — core `Browser`, `Page`, `Locator`, `Frame`, `BrowserContext`
- [`ferridriver-test`](https://docs.rs/ferridriver-test) — `TestRunner`, `#[ferritest]`, `expect`, fixtures, reporters
- [`ferridriver-bdd`](https://docs.rs/ferridriver-bdd) — `BrowserWorld`, step macros, registry, `bdd_main!()`
- [`ferridriver-mcp`](https://docs.rs/ferridriver-mcp) — `McpServer`, `McpServerConfig`, tool registry
- [`ferridriver-cli`](https://docs.rs/ferridriver-cli) — CLI binary crate
- [`ferridriver-test-macros`](https://docs.rs/ferridriver-test-macros)
- [`ferridriver-bdd-macros`](https://docs.rs/ferridriver-bdd-macros)

`ferridriver-node` is a `cdylib` for NAPI and is not published to crates.io.
