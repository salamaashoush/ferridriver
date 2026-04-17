# Parallelism and isolation

ferridriver's test runner is built around two hard rules:

1. **Every test gets a fresh `BrowserContext`** — storage, cookies, permissions, and network state are isolated.
2. **Every worker owns exactly one `Browser`** — one browser process per worker, for the whole run.

Everything else in the execution model follows from those two rules.

## Worker model

```
 run start
    │
    ├── Dispatcher (MPMC work-stealing channel)
    │
    ┌────────┬────────┬────────┬────────┐
    │   W0   │   W1   │   W2   │   W3   │      workers = 4
    │        │        │        │        │
    │ Browser│ Browser│ Browser│ Browser│      one per worker
    │   │    │   │    │   │    │   │    │
    │ ┌─▼─┐  │ ┌─▼─┐  │ ┌─▼─┐  │ ┌─▼─┐  │
    │ │Ctx│  │ │Ctx│  │ │Ctx│  │ │Ctx│  │      fresh per test
    │ │ │ │  │ │ │ │  │ │ │ │  │ │ │ │  │
    │ │Pg │  │ │Pg │  │ │Pg │  │ │Pg │  │
    │ └───┘  │ └───┘  │ └───┘  │ └───┘  │
    └────────┴────────┴────────┴────────┘
```

**Worker boot.** All N workers launch their browsers **concurrently** using `tokio::join!`, not sequentially. On a warm machine this saves 80–100 ms per additional worker because browser startup overlaps.

**Dispatch.** The dispatcher is an unbounded MPMC channel. Each test is enqueued once. Workers pull work as they finish; fast workers naturally pick up more. No thread-pool hashing, no per-worker queues to balance.

**Teardown.** Contexts close after each test (with optional screenshot on failure). The browser stays alive for the worker's entire run — browser launches are the most expensive thing you can do, so the model amortizes them.

## Configuring worker count

```toml
# ferridriver.config.toml
workers = 4
```

CLI flag: `-j N` / `--workers N`. Defaults to logical CPU count capped at some sane ceiling.

```bash
cargo test --test e2e -- -j 4
npx @ferridriver/test test -j 8
```

Under a CI runner with a fixed CPU budget, pin this. Letting the runner auto-scale on a shared host leads to unpredictable timings.

## Test isolation

Every `#[ferritest]` body receives a `TestContext` whose `page()` / `context()` / `browser()` are cached fixtures:

- `browser` — worker-scoped, shared across all tests on this worker
- `context` — test-scoped, **created fresh for this test**, torn down at end
- `page` — test-scoped, opened in the fresh context

This means:

- Cookies and localStorage from test A cannot leak into test B, even on the same worker.
- A failing `before_each` in test A does not poison test B's state — its context was never created.
- You can `page.context().add_cookies(...)` inside the test body with no cleanup logic; the context goes away when the test finishes.

## Parallel vs serial suites

By default all tests are *fully parallel*: the dispatcher treats every test as an independent work item.

Mark a suite `serial` when tests share external state (database rows, file locks, a specific login session) that can't be isolated per-test:

```rust
#[ferritest_suite(mode = "serial")]
mod payment_flow {
    use ferridriver_test::prelude::*;

    #[ferritest]
    async fn initiate_payment(ctx: TestContext) { /* ... */ }

    #[ferritest]
    async fn verify_receipt(ctx: TestContext) { /* runs only if above passed */ }
}
```

A serial suite is enqueued as a single `WorkItem::Serial` — one worker grabs the whole batch, runs tests in source order, and **skips the rest on first failure**. The other workers keep processing parallel tests from the queue.

## Sharding for CI

`--shard N/M` splits the test list into `M` roughly-equal shards and runs only shard `N`.

```bash
# Three GitHub Actions jobs, one per shard
npx @ferridriver/test test --shard 1/3
npx @ferridriver/test test --shard 2/3
npx @ferridriver/test test --shard 3/3
```

Sharding is deterministic given a stable test discovery order. Combine with JUnit output and a CI test-report merger (e.g. GitHub's built-in one) to aggregate results.

## Retries

A failing test is re-enqueued as a `WorkItem::Single` — any worker can pick it up, not necessarily the one that failed. `RetryPolicy::final_status` determines the outcome:

| Attempt history | Final status |
|---|---|
| All passed | `Passed` |
| Last passed, prior failed | `Flaky` (surfaced separately in reports) |
| Last failed | `Failed` |
| All skipped | `Skipped` |

This separation matters for flake detection: a `Flaky` test is not a *regression*, but it's also not silent — reporters surface the retry history so you can decide whether to investigate or quarantine.

```bash
cargo test --test e2e -- --retries 2        # 1 original + 2 retries
```

## Practical guidance

- **Start with `workers = 4`.** Four is almost always faster than one. Beyond 4, you start thrashing I/O and RAM on most laptops and small CI runners.
- **If tests are flaky at `workers = 8` but stable at `workers = 4`**, you have a hidden shared-state dependency (a DB row, a localStorage key, a login session). Find it; don't just lower the worker count.
- **Use `serial` sparingly.** It's the single most expensive escape hatch. Prefer per-test fixtures that isolate the shared state.
- **`--retries 2` in CI is fine.** Anything higher is a smell.
