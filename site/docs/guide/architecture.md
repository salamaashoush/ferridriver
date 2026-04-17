# Architecture

ferridriver is a single Rust engine wrapped in many shapes. The test runner, BDD framework, MCP server, and NAPI bindings don't ship their own browser logic — they all dispatch to the same core. That's where the consistency comes from, and it's also where the speed comes from.

## The layers

```mermaid
flowchart TB
  subgraph TS ["TypeScript"]
    A["@ferridriver/test"]
    B["@ferridriver/ct-{react,vue,svelte,solid}"]
  end

  N["@ferridriver/node (NAPI)"]

  subgraph RustTools ["Rust tools"]
    C["ferridriver-cli (MCP)"]
    D["ferridriver-test"]
    E["ferridriver-bdd"]
    F["ferridriver-mcp (28 tools)"]
  end

  Core["ferridriver (core)\nBrowser · Page · Locator · Frame · Context"]

  subgraph Backends
    direction LR
    B1["CdpPipe"]
    B2["CdpRaw"]
    B3["WebKit"]
    B4["Bidi"]
  end

  A --> N
  B --> A
  N --> Core
  C --> F
  F --> Core
  D --> Core
  E --> Core
  E --> D
  Core --> B1
  Core --> B2
  Core --> B3
  Core --> B4

  classDef tsLayer fill:#dbeafe,stroke:#1e40af,color:#0f172a
  classDef rustLayer fill:#fef3c7,stroke:#b45309,color:#1c1917
  classDef coreLayer fill:#dcfce7,stroke:#15803d,color:#052e16
  classDef backendLayer fill:#ede9fe,stroke:#6d28d9,color:#1e1b4b
  class A,B tsLayer
  class N,C,D,E,F rustLayer
  class Core coreLayer
  class B1,B2,B3,B4 backendLayer
```

The core holds the browser protocol knowledge — nothing above it has to. That's why a Gherkin step, a Rust `#[ferritest]`, and an MCP tool call all reach the same `Page::click` implementation.

## Backends at a glance

Four transports, one API:

| Backend | Browser | Transport | Default? |
|---|---|---|---|
| `cdp-pipe` | Chromium | CDP over fd 3/4 pipes | yes |
| `cdp-raw`  | Chromium | CDP over WebSocket    | |
| `webkit`   | WKWebView (macOS) | Native Obj-C IPC | |
| `bidi`     | Firefox | WebDriver BiDi | |

Backends dispatch through an `enum`, not a trait object. Each call monomorphizes to a single backend path — no vtable lookup, no dynamic dispatch, and the compiler can inline across the boundary. You pay for exactly one backend per process.

See [Concepts → Backends](/concepts/backends) for when to pick which.

## Test execution

Every test — E2E, BDD, component, NAPI — runs through one pipeline: `TestRunner::run()`. The BDD crate translates `.feature` files into the same `TestPlan`. The NAPI test runner delegates to the same Rust function. There is no second runner.

```mermaid
flowchart LR
  plan["TestPlan\n(suites + tests)"] --> filter["filter\nshard · grep · tag · only · last-failed"]
  filter --> dag["validate\nfixture DAG"]
  dag --> setup["run\nglobal setup"]
  setup --> disp(["Dispatcher\nMPMC work-stealing channel"])

  disp --> W0["Worker 0\nBrowser"]
  disp --> W1["Worker 1\nBrowser"]
  disp --> W2["Worker 2\nBrowser"]
  disp --> W3["Worker 3\nBrowser"]

  W0 --> collect["collect results\n+ retry failed tests"]
  W1 --> collect
  W2 --> collect
  W3 --> collect

  collect --> teardown["run\nglobal teardown"]
  teardown --> out["exit code"]

  classDef stage fill:#fef3c7,stroke:#b45309,color:#1c1917
  classDef worker fill:#dcfce7,stroke:#15803d,color:#052e16
  classDef router fill:#ede9fe,stroke:#6d28d9,color:#1e1b4b
  class plan,filter,dag,setup,collect,teardown,out stage
  class W0,W1,W2,W3 worker
  class disp router
```

A few consequences worth knowing:

- **Workers launch browsers concurrently**, not sequentially. On a warm machine, overlapping launches save 80–100 ms per extra worker.
- **The dispatcher is work-stealing**. Fast workers pick up more tests. You don't hand-balance anything.
- **Retry re-enqueues**. A failed test goes back on the shared queue, and any worker can grab it — not just the one that failed.

Dive deeper in [Concepts → Parallelism and isolation](/concepts/parallelism-and-isolation).

## A single test, end to end

What actually happens from the moment a worker pulls a test off the queue:

```mermaid
sequenceDiagram
  autonumber
  participant D as Dispatcher
  participant W as Worker
  participant B as Browser
  participant Ctx as BrowserContext
  participant P as Page
  participant T as Test body

  D->>W: WorkItem(test)
  W->>B: ensure launched (once per worker)
  W->>Ctx: create fresh context
  Ctx->>P: new page
  W->>W: inject fixtures (browser, context, page, test_info)
  W->>T: beforeEach(ctx)
  W->>T: run test body
  T->>P: actions + assertions (auto-wait)
  W->>T: afterEach(ctx)
  alt on failure
    W->>P: screenshot
  end
  W->>Ctx: close
  W->>D: result (Passed / Failed / Flaky / Skipped)
```

Three things to notice:

- The `Browser` survives between tests. The `BrowserContext` does not.
- `afterEach` runs even when the test body fails. That's how you keep teardown reliable.
- Retry is separate from this loop — a failed test goes back into the dispatcher; you see this same diagram play out again, possibly on a different worker.

## Why the shape is this shape

A few opinionated choices that fall out of the architecture:

- **One engine, many frontends.** Adding a new test style (a new macro, a new DSL, an MCP tool) doesn't fork the execution path. It translates into a `TestPlan` and lets the core handle the rest.
- **Rust owns the hot path.** Polling, actionability checks, selector compilation, CDP transport — all Rust. The TypeScript `expect` wrapper is a thin shim; it issues one NAPI call per assertion and the retry loop stays inside Rust.
- **Per-worker browser, per-test context.** Launching a browser is the most expensive thing you can do. Creating a context is cheap. Amortize the first, refresh the second.
- **Dispatch via enum, not trait object.** Uniform API without the vtable cost.

If you want the file-level map, see the [workspace section of the root README](https://github.com/salamaashoush/ferridriver#workspace).
