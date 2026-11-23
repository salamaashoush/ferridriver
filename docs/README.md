# Maintainer docs

Reference for people working on ferridriver. User-facing documentation
lives at <https://salamaashoush.github.io/ferridriver/>
(source: [`site/`](../site/)).

- [`rust-testing.md`](./rust-testing.md) — authoring E2E tests in Rust:
  harness setup, `#[ferritest]` fixture parameters, custom fixtures,
  suites/hooks, action builders, `expect`, and runtime flags.
- [`extensions.md`](./extensions.md) — the authoring contract for JS / TS
  extension files (manifest, capabilities, `allow.commands`, `allow.net`,
  hooks, World, sandbox guarantees).
- [`playwright-compat.md`](./playwright-compat.md) — the compatibility
  harness behind `just compat`: how a spec is run against both runners
  and how a divergence is attributed.
- [`backend-quirks.md`](./backend-quirks.md) — undocumented browser,
  protocol and binding behaviour, by backend. Read before changing one.
- [`protocol-deadlines.md`](./protocol-deadlines.md) — how a native
  `page.webmcp` operation's single deadline is shared across backends.
- [`android-device-api.md`](./android-device-api.md) — the `_android`
  device API over an ADB server.
- [`managed-android.md`](./managed-android.md) and
  [`managed-ios.md`](./managed-ios.md) — Android browsers and iOS Safari
  as managed instance targets.

Only reference material belongs here. Handover notes, backlogs and design
rationale rot the moment the code moves on; that context belongs in the
commit that made the change, or in the tracker.
