# Maintainer docs

Living documents for ferridriver maintainers. User-facing documentation
lives at <https://salamaashoush.github.io/ferridriver/>
(source: [`site/`](../site/)).

## Living

- [`rust-testing.md`](./rust-testing.md) — authoring E2E tests in Rust:
  harness setup, `#[ferritest]` fixture parameters, custom fixtures,
  suites/hooks, action builders, `expect`, and runtime flags.
- [`extension-architecture.md`](./extension-architecture.md) — the design notes
  behind the JS / TS extension system: how it compares against VS Code /
  Deno / WASM / Rollup, what we adopted, what we deferred and why.
- [`extensions.md`](./extensions.md) — the authoring contract for JS / TS
  extension files (manifest, capabilities, `allow.commands`, `allow.net`,
  hooks, World, sandbox guarantees).
