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

Only reference material belongs here. Handover notes, backlogs and design
rationale rot the moment the code moves on; that context belongs in the
commit that made the change, or in the tracker.
