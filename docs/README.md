# Maintainer docs

Living documents for ferridriver maintainers. User-facing documentation
lives at <https://salamaashoush.github.io/ferridriver/>
(source: [`site/`](../site/)).

## Living

- [`plugin-architecture.md`](./plugin-architecture.md) — the design notes
  behind the JS / TS extension system: how it compares against VS Code /
  Deno / WASM / Rollup, what we adopted, what we deferred and why.
- [`extensions.md`](./extensions.md) — the authoring contract for JS / TS
  extension files (manifest, capabilities, `allow.commands`, `allow.net`,
  hooks, World, sandbox guarantees).

## Archived

[`archive/`](./archive/) — phase notes and completed migrations. Kept for
historical context; not authoritative for the current codebase.

- `phase6-pw-webkit.md` — plan to replace the native WKWebView / webkit6
  backends with Playwright WebKit. Completed: the `webkit` backend now
  speaks the Playwright Inspector protocol on every platform.
- `webkit-linux-port.md` — research notes for the original webkit6 / GTK4
  Linux host. Superseded by the Playwright WebKit migration above.
- `webkit-linux-port-handoff.md` — session handoff for the webkit6 port.
  Superseded.
