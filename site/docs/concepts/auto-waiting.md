# Auto-waiting

Test flake usually comes from one of three places: clicking before the
target is clickable, asserting before the DOM catches up, or racing a
navigation. ferridriver handles all three for you — no `sleep(200)`, no
manual `waitForSelector` dance.

There are **two layers** of waiting, and they compose:

1. **Actionability checks** — run before every action (`click`, `fill`,
   `hover`, `press`, …). Runs for at most 5 seconds, polling every 50 ms.
2. **`expect` polling** — runs on assertions (`to_be_visible`,
   `to_have_text`, …). Uses Playwright's interval schedule
   `[100, 250, 500, 1000, 1000, ...]` up to `expectTimeout` (default
   5000 ms).

## Actionability — before actions

Before executing any action, the Rust core asks the browser to evaluate
`window.__fd.isActionable(el)`. An element is actionable when it is:

- **Attached** (`el.isConnected`)
- **Visible** (non-zero box; `display` / `visibility` / `opacity` pass)
- **Enabled** (not `aria-disabled`)
- **Stable** (bounding box unchanged across animation frames — an element
  mid-transition keeps polling until it settles)
- **Receives events** (for clicks: the click point hit-tests to the target
  and is not occluded by another element)

This is the same set Playwright enforces. The exact subset depends on the
action — `hover` waits for visible + stable, `fill` for visible + enabled
+ editable, `click` for visible + enabled + stable plus the hit-test. If
any check fails, the Rust side yields for 50 ms and retries. After
5 seconds the call fails with `Timeout: element not actionable`.

```rust
// No manual wait — fill() waits for #email to be attached, visible, enabled.
page.locator("#email").fill("user@example.com").await?;
```

The poll loop is in Rust, not JavaScript (`tokio::time::sleep`). Other
pages in the same worker keep making progress while one element is
waiting — there is no blocking JS promise holding up the event loop.

## `expect` polling — on assertions

`expect(...)` returns an assertion builder. Calling a matcher starts a
retry loop in the Rust core:

```
attempt #1   check immediately
attempt #2   wait 100 ms, check
attempt #3   wait 250 ms, check
attempt #4   wait 500 ms, check
attempt #5+  wait 1000 ms, check  (cycles)
```

The loop stops when the assertion passes or `expectTimeout` elapses
(default 5000 ms). The schedule matches Playwright so test durations are
portable.

```rust
use ferridriver_test::prelude::*;

expect(&page.locator("#banner"))
    .to_be_visible()
    .await?;                                    // polls for up to 5s

expect(&page.locator(".row"))
    .to_have_count(10)
    .with_timeout(Duration::from_secs(30))      // extend for slow endpoints
    .await?;
```

TypeScript keeps the same schedule under the hood — the matcher is a
single NAPI call per assertion and the retry loop stays inside Rust.

## Negation (`.not`)

Negating a matcher flips the success condition but keeps the polling.
`expect(&loc).not().to_be_visible()` waits for the element to
*disappear*. Same cadence, same timeout.

## Soft assertions

`.soft()` records a failure but does not stop the test. Useful for
collecting multiple independent checks in one pass:

```rust
expect(&page).to_have_title("Dashboard").soft().with_message("title").await?;
expect(&page.locator("#user")).to_have_text("Ada").soft().await?;
expect(&page.locator("#logout")).to_be_visible().soft().await?;
// All three run; failures aggregate and the test fails at the end if any failed.
```

## Navigations

`page.goto(url, None)` resolves on `load` by default. Override with
`GotoOptions { wait_until: Some("networkidle".into()), .. }` —
`wait_until` is a string: `"load"` (default), `"domcontentloaded"`,
`"networkidle"`, or `"commit"`.

For navigations triggered by clicks, use `page.expect_navigation` which
registers the listener before the action and awaits it afterwards — no
lost-event races.

## When to extend timeouts

The defaults (5 s actionability, 5 s expect) are tuned for interactive
apps. Bump them for:

- Heavy server-rendered pages that finish painting only after multiple XHRs.
- CI machines that are occasionally 2–3× slower than laptops.
- Tests that wait on real backends (batch jobs, queue-processing UIs).

Prefer raising `expectTimeout` or `timeout` (per-test) over adding
manual `sleep` calls. Polling short-circuits as soon as the condition
passes; a blind sleep always waits the full duration.

## `force` — skipping the checks

Pass `force: true` to bypass actionability entirely and dispatch the
action immediately, same as Playwright. Use it only when you deliberately
want to act on a not-yet-actionable element (e.g. asserting a disabled
button does nothing).

```rust
use ferridriver::options::ClickOptions;

page.locator("#submit")
    .click(Some(ClickOptions { force: Some(true), ..Default::default() }))
    .await?;
```

The `stable` gate is `requestAnimationFrame`-driven. On a backgrounded
page rAF is throttled, so the gate races a `setTimeout` watchdog: if it
hasn't settled within ~1 s the element is treated as stable (a page whose
rAF is paused is not animating). Foreground pages settle in one frame, so
there is no added latency in the common case.
