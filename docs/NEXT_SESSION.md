# Next session — §2.12 ConsoleMessage rich

Tier 1 done. §3.1, §3.12, §2.9, §2.11, §2.10 landed. Next pick:
**§2.12 ConsoleMessage rich** — replace the wire-shaped
`ConsoleMsg { type, text }` with the full Playwright
`ConsoleMessage { args, location, page, text, type, timestamp }`.

## Read-first

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — §2.12 is next; §2.10 just landed.
3. `HANDOVER.md` — §2.10 Download summary (load-bearing — the
   `DownloadManager` + `watch::send_replace` pattern generalises
   to any future live-handle with terminal state).
4. `/tmp/playwright/packages/playwright-core/src/client/consoleMessage.ts`
   + `/tmp/playwright/packages/playwright-core/src/server/console.ts`.

## §2.12 canonical surface

```ts
class ConsoleMessage {
  args(): JSHandle[];
  location(): { url: string; lineNumber: number; columnNumber: number };
  page(): Page | null;
  text(): string;
  type(): ConsoleMessageType;
}

// event: page.on('console', (msg: ConsoleMessage) => { ... })
```

`ConsoleMessageType` union includes
`'log' | 'debug' | 'info' | 'error' | 'warning' | 'dir' | 'dirxml' |
 'table' | 'trace' | 'clear' | 'startGroup' | 'startGroupCollapsed' |
 'endGroup' | 'assert' | 'profile' | 'profileEnd' | 'count' |
 'timeEnd'`.

## Implementation sketch (generalise §2.10)

1. **Rust core — new `crates/ferridriver/src/console_message.rs`**:
   - `ConsoleMessage` struct behind `Arc` with sync getters.
   - `args` is `Vec<JSHandle>` — each arg was a `Runtime.RemoteObject`
     in CDP (or a BiDi equivalent). Blocks on §1.3 (`JSHandle` rich
     shape) — if nested-arg walking is still incomplete, shipping a
     flat `args: Vec<JSHandle>` keyed off top-level objectIds is OK
     for now, Rule-9 test just asserts `args.length === 2` +
     `args[0].asElement()` for an element arg.
   - `location` = `{ url, line, column }` — CDP
     `Runtime.consoleAPICalled` carries `stackTrace.callFrames[0]`;
     BiDi `log.entryAdded` carries `source.url|lineNumber|columnNumber`.
     WebKit's console interceptor currently reports the text only —
     stack frames are a real gap to add.

2. **Per-backend upgrades**:
   - **CDP** (`backend/cdp/mod.rs::spawn_console_listener`): already
     reads `Runtime.consoleAPICalled.args[i].value`. Upgrade to
     build `JSHandle`s from `args[i]` (keep the `objectId` for
     remote-backed handles, inline `value` for value-backed). Include
     `stackTrace` for location.
   - **BiDi** (`backend/bidi/page.rs`): `log.entryAdded` with
     `type: 'console'` carries `args` (serializable `RemoteValue`s) +
     `source: { url, lineNumber, columnNumber }`. Reuse the §1.3 BiDi
     handle builder.
   - **WebKit** (`backend/webkit/mod.rs`): console currently reports
     only `(level, text)` — the host's JS interceptor logs to the
     webview's `console` protocol. Extending to args requires a new
     IPC op that captures each arg's serialization; left as a gap
     under §B if the scope is too large for one commit.

3. **Event flow**:
   - `PageEvent::Console(ConsoleMsg)` → `PageEvent::Console(ConsoleMessage)`.
   - Update NAPI `Either7` path — add a `ConsoleMessage` class OR
     route through the existing JSON snapshot path if a live class
     isn't needed (Playwright returns a live class).

4. **NAPI / QuickJS**:
   - `#[napi] class ConsoleMessage` with `args()` / `location()` /
     `text()` / `type()`. Union the `Either7` into `Either8` adding
     `ConsoleMessage`.
   - QuickJS `ConsoleMessageJs` same surface.

5. **Rule-9 integration tests**:
   - Page-side `console.log('hello', {foo:1}, document.body)`.
   - Observer: `msg.text() === "hello [object Object] [object HTMLBodyElement]"`
     (Playwright's stringification), `msg.type() === 'log'`,
     `msg.args().length === 3`, `msg.args()[2].asElement()` returns
     a `<body>` handle.
   - Per-backend on all four. WebKit may be partial — document
     honestly under §B if so.

## Ground rules (from CLAUDE.md)

- Rule 1/2: core is source of truth; three layers update in the same commit.
- Rule 3: no wire shapes leak — args are `JSHandle`, not `RemoteObject`.
- Rule 4: every backend real — typed `Unsupported` only where the
  protocol genuinely can't.
- Rule 6: read `/tmp/playwright/...` before each signature.
- Rule 7: rebuild NAPI + diff `.d.ts` against Playwright's `test.d.ts`.
- Rule 9: per-backend integration test before flipping `[x]`.
- Rule 10: no escape hatches.

## Baseline (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p ferridriver --lib                                 # 125 core
cargo test -p ferridriver-script --lib                          # 22 script
cargo test -p ferridriver-mcp --lib                             # 38 MCP
cd crates/ferridriver-node && bun run build:debug
cd <repo root> && bun test                                      # 825
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# cdp-pipe 132, cdp-raw 132, bidi 127, webkit 128
```

## Commit shape

Single commit: `feat(page): ConsoleMessage as first-class handle (§2.12)`.
Update `PLAYWRIGHT_COMPAT.md` §2.12 to `[x]` and rewrite
`HANDOVER.md` + `docs/NEXT_SESSION.md` in the same commit.

## Notes from §2.10 that generalise

- **`tokio::sync::watch::Sender::send` silently discards the value
  when `receiver_count() == 0`**. For any one-shot terminal-state
  transition on a handle whose consumer subscribes lazily (like
  `download.path()` on a download emitted via `page.on('download')`),
  use `send_replace` — it always updates the internal state. This
  was an hour-long hang in the §2.10 session before we traced it.
  If you use `watch` for another terminal-state handle, same rule
  applies.
- **`PageBackref` is shared infra** (`backend/mod.rs`). Any new
  live-handle that needs an `Arc<Page>` from a backend async task
  can reuse it as-is.
- **`DownloadManager` is the third clone of the
  `DialogManager` / `FileChooserManager` pattern**. If you're
  adding a fourth (`WebError`, `ConsoleMessage` broadcast, etc.),
  consider whether it's worth pulling out a generic
  `HandleManager<T>` — the code is 95% identical across the three.
  For §2.12 specifically it's probably fine to keep inlining;
  generalisation may belong in §2.13 / §2.14.
- **Per-backend temp dirs**: downloads land in a per-page `Arc<TempDir>`
  owned by each backend's `Page` struct. The `TempDir` drop cleans
  up orphans on page close. Same pattern would work for
  `record_har`, `record_video` paths (both in §4.x).
- **QuickJS has no `setTimeout`** (still — unchanged from §2.11).
  When a `run_script` test needs to observe an async DOM mutation,
  poll from the **page context** via
  `await page.evaluate(async (arg) => { ... await new Promise(r =>
  setTimeout(r, 10)); ... }, arg)`. See `WAIT_FOR_TITLE_CALL` in
  `tests/backends_support/file_chooser.rs`.
