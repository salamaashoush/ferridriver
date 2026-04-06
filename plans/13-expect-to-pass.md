# Feature: expect.toPass() Auto-Retry

## Context
Some assertions depend on asynchronous state changes that don't have a clear locator-based wait. `expect.toPass()` wraps an assertion block and retries it until it passes or times out. This is safer than `sleep()` and more flexible than locator auto-wait. Playwright added this as a generic retry wrapper for any assertion logic.

## Design

### Architecture
| Crate/Package | Role |
|---|---|
| `ferridriver-test` | `expect_to_pass()` function with retry logic |
| `ferridriver-bdd` | BDD step for polling assertions |
| `packages/ferridriver-test` | `expect(fn).toPass()` TS API |
| `ferridriver-napi` | Expose retry logic to TS |

### Core Changes (ferridriver-test)
- New function in `crates/ferridriver-test/src/expect/mod.rs` (or new file `expect/to_pass.rs`):
  ```rust
  pub async fn expect_to_pass<F, Fut>(
    block: F,
    options: ToPassOptions,
  ) -> Result<(), TestFailure>
  where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<(), TestFailure>>,
  ```
- `ToPassOptions`:
  ```rust
  pub struct ToPassOptions {
    pub timeout: Duration,           // default: 5s
    pub intervals: Vec<Duration>,    // default: [100ms, 250ms, 500ms, 1000ms]
    pub message: Option<String>,     // custom error message on final failure
  }
  ```
- Behavior:
  1. Call `block()`. If it returns `Ok(())`, return immediately.
  2. Wait `intervals[i]` (cycling through intervals if exhausted).
  3. Call `block()` again. Repeat until success or `timeout` elapsed.
  4. On timeout: return the last error from the block, wrapped with context about retry count.
- The block is called with `catch_unwind` semantics — panics from assertions are caught and treated as failures.
- Integration with `TestInfo`: emit step events for each retry attempt (useful for trace/debugging).

### BDD Integration (ferridriver-bdd)
- Step: `Then within {int} seconds, {step}` — retries the inner step until it passes.
  - Example: `Then within 5 seconds, the element "#status" should have text "Complete"`
- Alternative: `Then the following should pass within {int} seconds:` with doc string of steps.
- Implementation: parse inner step, retry with `expect_to_pass`.

### NAPI + TypeScript (ferridriver-napi, packages/ferridriver-test)
- API on the expect chain:
  ```ts
  await expect(async () => {
    const count = await page.locator('.item').count();
    expect(count).toBe(5);
  }).toPass({ timeout: 10_000, intervals: [500, 1000, 2000] });
  ```
- Implementation: `toPass` is a method on the expect wrapper that calls the Rust `expect_to_pass` via NAPI.
- Alternative pure-TS implementation: loop in JS, catch errors, retry. This avoids NAPI round-trips per retry.
  - **Go with pure-TS** for simplicity — the retry logic is trivial JS.

### CLI (ferridriver-cli)
- No new flags. Timeout configurable via `expect_timeout` in config.

### Component Testing (ferridriver-ct-*)
- Works identically. Useful for waiting on component re-renders:
  ```ts
  await expect(async () => {
    await expect(page.locator('.counter')).toHaveText('5');
  }).toPass();
  ```

## Implementation Steps
1. Create `crates/ferridriver-test/src/expect/to_pass.rs` with `expect_to_pass` function.
2. Implement retry loop with configurable intervals and timeout.
3. Add `ToPassOptions` with sensible defaults.
4. Wrap assertion panics with `catch_unwind` (or `AssertUnwindSafe` for async).
5. Add step event emission for retry attempts.
6. Implement BDD step: `Then within {int} seconds, {step}`.
7. Implement TS `expect(fn).toPass()` in `packages/ferridriver-test/src/expect.ts`.
8. Write tests with async state changes.

## Key Files
| File | Action |
|---|---|
| `crates/ferridriver-test/src/expect/to_pass.rs` | Create |
| `crates/ferridriver-test/src/expect/mod.rs` | Modify — export `to_pass` |
| `crates/ferridriver-bdd/src/steps/` | Modify — add retry step |
| `packages/ferridriver-test/src/expect.ts` | Modify — add `toPass()` method |

## Verification
- Unit test: block that fails 3 times then succeeds -> `expect_to_pass` returns Ok.
- Unit test: block that always fails -> times out with last error after configured timeout.
- Unit test: verify retry intervals are respected (mock timer or measure elapsed).
- BDD test: `Then within 3 seconds, the element "#status" should have text "Done"` with delayed DOM update.
- TS test: `expect(async () => { ... }).toPass({ timeout: 5000 })` succeeds after retries.
