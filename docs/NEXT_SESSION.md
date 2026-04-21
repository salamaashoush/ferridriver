# Next session — §2.9 Dialog as first-class handle + page.on('dialog')

Tier 1 is fully [x]. §3.1 (navigation returns Response) is fully [x]
from this cycle. The next pick from the high-usage Playwright queue is
`§2.9 Dialog` — promoting Dialog from our current callback-based
`set_dialog_handler` to a first-class event handle matching Playwright's
`page.on('dialog', dialog => dialog.accept())` shape.

## Read-first

1. `CLAUDE.md` — Playwright-parity rules (Rules 1–10) and the
   consolidated lessons. Authoritative cross-device source.
2. `PLAYWRIGHT_COMPAT.md` — gap tracker. §2.9 is the next item.
3. `HANDOVER.md` — full block-level summary of what landed in §3.1.
4. `/tmp/playwright/packages/playwright-core/src/client/dialog.ts`
   — canonical client surface.
5. `/tmp/playwright/packages/playwright-core/src/server/dialog.ts`
   (server-side protocol shape).

## §2.9 scope (canonical Playwright signature)

```ts
class Dialog {
  type(): string;                   // 'alert' | 'confirm' | 'prompt' | 'beforeunload'
  message(): string;
  defaultValue(): string;
  accept(promptText?: string): Promise<void>;
  dismiss(): Promise<void>;
  page(): Page | null;
}

// Event wiring:
page.on('dialog', (d: Dialog) => { ... });
```

Important semantics:

- **One-shot**: each Dialog must be accepted or dismissed exactly once.
  Double-accept/double-dismiss must error (matches Playwright).
- **Beforeunload auto-dismiss**: if no `dialog` listener is registered
  at the time `beforeunload` fires, the dialog must be automatically
  dismissed (otherwise the page hangs). All other dialog types with
  no listener: auto-dismiss too (Playwright: "dialogs are dismissed
  automatically unless there's a listener").
- **Default value**: `'prompt'` dialogs carry the default text in the
  HTML input; accept with `promptText` overrides it.

## Architecture sketch

1. **Rust core** — delete `page.set_dialog_handler` and its
   `DialogHandler` type. New `crates/ferridriver/src/dialog.rs` with:
   - `Dialog` struct wrapping `Arc<DialogState>`.
   - `DialogState { dialog_type, message, default_value, page_weak,
     responded: AtomicBool, responder: Arc<dyn Fn(DialogResponse) +
     Send + Sync> }`.
   - `Dialog::accept(prompt_text) -> Result<()>` — flip `responded`,
     dispatch the responder with `Accept { prompt_text }`. Double-accept
     errors.
   - `Dialog::dismiss()` — same pattern, `Dismiss`.
   - Auto-dismiss: each backend's dialog listener, after emitting
     `PageEvent::Dialog(handle)`, starts a short grace task (say one
     event-loop tick) — if the emitter has zero listeners OR the
     emitted Dialog is not `responded`, dismiss it. For `beforeunload`
     the grace must be immediate / before the navigation unpauses.

2. **CDP backend** (`backend/cdp/mod.rs`) — the existing
   `Page.javascriptDialogOpening` listener constructs a `DialogState`
   that, on `accept`/`dismiss`, sends `Page.handleJavaScriptDialog`
   with the right action. Emit `PageEvent::Dialog(Dialog { state })`.

3. **BiDi backend** (`backend/bidi/page.rs`) — already handles
   `browsingContext.userPromptOpened`. Change it from a
   `dialog_handler`-based auto-respond into emitting
   `PageEvent::Dialog(...)` with the responder calling
   `browsingContext.handleUserPrompt`.

4. **WebKit backend** (`backend/webkit/mod.rs`) — the Obj-C host
   reports alert/confirm/prompt via IPC. Thread through the same
   `Dialog` wrapper. `beforeunload` is tricky on WKWebView — document
   the limit if there's no public API.

5. **NAPI** — `#[napi] class Dialog` with the four methods.
   `page.on('dialog', cb)` event subscription already routed via
   `EventEmitter`; wire the `Dialog` type into the existing
   `PageEvent` → JS event dispatcher.

6. **QuickJS** — `DialogJs` class in `bindings/dialog.rs`. Register.
   `page.on('dialog', cb)` dispatches the JS callback with a live
   wrapper.

## Per-backend Rule-9 integration tests

In a new `crates/ferridriver-cli/tests/backends_support/dialog.rs`:

1. `test_dialog_accept_alert` — page runs `alert('hi')`, listener
   accepts, `alert()` returns to page JS.
2. `test_dialog_accept_prompt_with_text` — page runs
   `prompt('name?', 'alice')`, listener inspects `defaultValue() ===
   'alice'` then `accept('bob')`, page JS sees `'bob'`.
3. `test_dialog_dismiss_confirm` — page runs `confirm('ok?')`,
   listener dismisses, `confirm()` returns `false`.
4. `test_dialog_auto_dismiss_no_listener` — remove all `dialog`
   listeners, page runs `alert('hi')`; dialog auto-dismisses, page
   JS continues within bounded time.
5. `test_dialog_beforeunload` — page with a `beforeunload` handler;
   without listener → auto-dismiss (navigation proceeds); with
   listener + accept → navigation proceeds; with listener + dismiss
   → navigation is canceled.
6. `test_dialog_double_accept_errors` — second `accept()` on the same
   Dialog rejects with a typed error.

Matching NAPI tests in `crates/ferridriver-node/test/dialog.test.ts`
covering at least items 1, 2, 3, and 4.

## Ground rules (from CLAUDE.md — non-negotiable)

- Rule 1: core is source of truth; bindings are thin delegators.
- Rule 2: all three layers (Rust core / NAPI / QuickJS) update in the same commit.
- Rule 4: every backend real — typed `FerriError::Unsupported` only
  where the protocol genuinely can't (e.g. `beforeunload` on
  WKWebView if stock public API has no hook).
- Rule 6: read `/tmp/playwright/...` before coding each signature.
- Rule 7: rebuild NAPI + diff generated `index.d.ts` against
  `/tmp/playwright/packages/playwright/types/test.d.ts` after every
  binding change.
- Rule 9: per-backend integration test on every backend before
  flipping `[x]`. No silent `if backend == ...` skips — typed
  `Unsupported` is the documented Rule-4 path.
- Rule 10: no `#[allow(clippy::*)]` escape hatches.
- No emojis, no AI attribution in commit messages, no task / phase
  / rule-number annotations in source comments or filenames.

## Baseline (must stay green through §2.9)

```
cargo clippy --workspace --all-targets -- -D warnings   # clean
cargo test --workspace --lib                            # 122 core
cd crates/ferridriver-node && bun run build:debug && bun test   # 781 bun
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1   # 4/4 backends
```

## Commit shape

Single commit:
- `feat(page): Dialog as first-class handle + page.on('dialog') (§2.9)` —
  body lists the surface, per-backend implementation (CDP via
  `Page.handleJavaScriptDialog`, BiDi via `browsingContext.handleUserPrompt`,
  WebKit via existing IPC), the `set_dialog_handler` removal, test
  coverage, and the `PLAYWRIGHT_COMPAT.md` §2.9 flip.

Update `PLAYWRIGHT_COMPAT.md` + `HANDOVER.md` in the same commit.

## Useful key locations

| area | path |
|---|---|
| Existing dialog handler (to replace) | `crates/ferridriver/src/page.rs` (grep `set_dialog_handler`) |
| CDP dialog listener | `crates/ferridriver/src/backend/cdp/mod.rs::spawn_dialog_listener` |
| BiDi dialog listener | `crates/ferridriver/src/backend/bidi/page.rs` (grep `userPromptOpened`) |
| WebKit dialog IPC | `crates/ferridriver/src/backend/webkit/ipc.rs`, `host.m` |
| Event emitter + PageEvent | `crates/ferridriver/src/events.rs` |
| NAPI entry | `crates/ferridriver-node/src/lib.rs` |
| QuickJS entry | `crates/ferridriver-script/src/bindings/mod.rs` |
| Per-backend integration tests | `crates/ferridriver-cli/tests/backends_support/` |
| Rules + lessons | `CLAUDE.md` |
