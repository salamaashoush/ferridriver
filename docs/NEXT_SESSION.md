# Next session — §2.13 WebError

Tier 1 done. §3.1, §3.12, §2.9, §2.11, §2.10, §2.12 landed. Next pick:
**§2.13 WebError** — live handle for `context.on('weberror')`
carrying the page's unhandled `Error` + page back-reference.

## Read-first

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — §2.13 is next; §2.12 just landed.
3. `HANDOVER.md` — §2.12 ConsoleMessage summary.
4. `/tmp/playwright/packages/playwright-core/src/client/webError.ts`
   + `/tmp/playwright/packages/playwright-core/src/server/` for the
   server-side emission path.

## §2.13 canonical surface

```ts
class WebError {
  page(): Page | null;
  error(): Error;
}

// event: context.on('weberror', (err: WebError) => { ... })
```

WebError is emitted on the *context*, not the page — unhandled
errors and promise rejections in any of the context's pages route
there. Playwright's `Page` also has `page.on('pageerror')` which is
the page-scoped equivalent; the two surfaces share the same underlying
event.

## Implementation sketch (generalise §2.12)

1. **Rust core — new `crates/ferridriver/src/web_error.rs`**:
   - `WebError` struct behind `Arc` with `page() -> Option<Arc<Page>>`
     and `error() -> &ErrorDetails { name, message, stack }`.
     Reusing `JsError` / existing error types is fine if they have
     the right shape; otherwise a dedicated `ErrorDetails` struct.
   - `WebErrorManager` or direct emission via `PageEvent::PageError`
     (already exists — just upgrade the variant from `String` to
     `WebError`).
   - Context-level fanout: ferridriver currently has
     `PageEvent::PageError(String)` on the page emitter. For parity
     we need both `page.on('pageerror', cb)` AND
     `context.on('weberror', cb)`. The simplest path: keep the
     per-page emission, and add a context-side bridge that forwards.

2. **Per-backend listeners**:
   - **CDP** (`backend/cdp/mod.rs`): `Runtime.exceptionThrown` — already
     has partial wiring for `PageEvent::PageError`. Upgrade to full
     `Error` object (name, message, stack from the exception details).
   - **BiDi** (`backend/bidi/page.rs`): `log.entryAdded` with
     `type: 'javascript'` already logs as page error. Same upgrade.
   - **WebKit** (`backend/webkit/mod.rs`): host interceptor may only
     surface `(level, text)` for JS errors — if stack traces aren't
     available through IPC, document Section B gap.

3. **NAPI / QuickJS**:
   - `#[napi] class WebError` with `page()` / `error()`.
   - Extend `waitForEvent` union `Either8 -> Either9` adding
     `WebError`.
   - `context.on('weberror', cb)` wire-up on `BrowserContextJs`.

4. **Rule-9 integration tests**:
   - Page-side `throw new Error('boom')` from inline `<script>`.
   - Observe via `context.waitForEvent('weberror')`:
     `err.error().message === 'boom'`, `err.error().name === 'Error'`,
     `err.page()` returns the page where the error originated.
   - Per-backend on all four. Document any backend gap honestly.

## Ground rules (from CLAUDE.md)

- Rule 1/2/3: core is source of truth; three layers update in the
  same commit; no wire shapes leak.
- Rule 4: every backend real.
- Rule 6: read `/tmp/playwright/...` first.
- Rule 7: rebuild NAPI + diff `.d.ts` against Playwright's types.
- Rule 9: per-backend integration test before flipping `[x]`.
- Rule 10: no escape hatches.

## Baseline (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p ferridriver --lib                                 # 125 core
cargo test -p ferridriver-script --lib                          # 13 script
cargo test -p ferridriver-mcp --lib                             # 38 MCP
cd crates/ferridriver-node && bun run build:debug
cd <repo root> && bun test                                      # 835
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# cdp-pipe 136, cdp-raw 136, bidi 131, webkit 132
```

## Commit shape

Single commit: `feat(context): WebError as first-class handle (§2.13)`.
Update `PLAYWRIGHT_COMPAT.md` §2.13 to `[x]` and rewrite `HANDOVER.md`
+ `docs/NEXT_SESSION.md` in the same commit.

## Notes from §2.12 that generalise

- **`tokio::sync::watch::Sender::send` vs `send_replace`** — still
  load-bearing. For any one-shot terminal-state transition on a
  handle whose consumer subscribes lazily, use `send_replace`.
- **`cdp_remote_object_to_backing` / `bidi_remote_value_to_backing`**
  helpers are the canonical protocol-wire-value -> `JSHandleBacking`
  shape for a future §2.13 / §2.14 that carries a `JSHandle`-typed
  arg. Reuse verbatim.
- **Inline `<script>` vs `page.evaluate` for stack-trace attribution**
  — CDP's `Runtime.consoleAPICalled.stackTrace` has empty frames for
  devtools-evaluate calls (no user-script source URL). Rule-9 tests
  that need a real stack trace must trigger via an inline `<script>`
  block in a navigated HTML document.
- **WebKit `(level, text)` IPC limitation** — host interceptor's
  current payload is the ceiling for console + WebError richness.
  Future phase: add a new `Op::ConsoleEvent` that carries
  args + stack-trace frames + `isError: bool` so console and
  WebError both benefit from the same IPC extension.
- **Section B gap paragraph style** — document backend limits in the
  field's doc comment (rustdoc visible) AND in PLAYWRIGHT_COMPAT.md
  §B, with the concrete symptom observable users see (timeout, empty
  vec, default value). Don't leave gaps implicit.
