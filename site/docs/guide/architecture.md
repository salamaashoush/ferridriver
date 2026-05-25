# Architecture

ferridriver is a single Rust engine wrapped in many shapes. The test
runner, BDD framework, MCP server, and NAPI binding don't ship their own
browser logic — they all dispatch to the same core. That is where the
consistency comes from, and it is also where the speed comes from.

## Layers

```mermaid
flowchart TB
  N["@ferridriver/node (NAPI core binding)"]

  subgraph RustTools [Rust tools]
    C["ferridriver-cli (mcp · bdd · test · run · install)"]
    D["ferridriver-test (TestRunner)"]
    E["ferridriver-bdd"]
    EX["ferridriver-expect"]
    F["ferridriver-mcp (10 tools)"]
    S["ferridriver-script (QuickJS: run_script · JS/TS BDD steps · extensions)"]
  end

  Core["ferridriver (core)\nBrowser · Page · Locator · Frame · Context · ElementHandle"]

  subgraph Backends
    direction LR
    B1["CdpPipe (default)"]
    B2["CdpRaw"]
    B3["WebKit (Playwright)"]
    B4["Bidi (Firefox)"]
  end

  N --> Core
  C --> F
  C --> E
  C --> S
  F --> Core
  F --> S
  E --> D
  E --> S
  D --> Core
  D --> EX
  EX --> Core
  S --> Core
  Core --> B1
  Core --> B2
  Core --> B3
  Core --> B4

  classDef rustLayer fill:#fef3c7,stroke:#b45309,color:#1c1917
  classDef coreLayer fill:#dcfce7,stroke:#15803d,color:#052e16
  classDef backendLayer fill:#ede9fe,stroke:#6d28d9,color:#1e1b4b
  class N,C,D,E,EX,F,S rustLayer
  class Core coreLayer
  class B1,B2,B3,B4 backendLayer
```

A Gherkin step, a Rust `#[ferritest]`, an MCP tool call, and a Node.js
`page.click()` all reach the same `Page::click` in `ferridriver`.

## Backends at a glance

| Backend     | Browser            | Transport                                       | Default? |
|-------------|--------------------|--------------------------------------------------|----------|
| `cdp-pipe`  | Chromium / Chrome  | CDP over Unix pipes (fd 3/4)                     | yes      |
| `cdp-raw`   | Chromium / Chrome  | CDP over WebSocket (also supports `Browser::connect`) |     |
| `webkit`    | Playwright WebKit  | Playwright Inspector protocol over `pw_run.sh` (NUL-delimited JSON) | |
| `bidi`      | Firefox            | WebDriver BiDi over WebSocket                    |          |

Backends dispatch through a Rust `enum` (`BackendKind`), not a trait
object. Calls monomorphize to a single backend path — no vtable lookup,
the compiler can inline across the boundary. You pay for exactly one
backend per process.

See [Concepts → Backends](/concepts/backends) for when to pick which.

## Test execution

Every test — Rust `#[ferritest]`, parameterized `#[ferritest_each]`, and
BDD scenarios — runs through one pipeline: `TestRunner::run()`. The BDD
crate translates `.feature` files into the same `TestPlan`; JavaScript /
TypeScript step bodies execute on the embedded QuickJS engine inside that
pipeline. There is no second runner.

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

A few consequences:

- **Workers launch browsers concurrently** via `tokio::join!`, not
  sequentially. On a warm machine, overlapping launches save 80–100 ms
  per extra worker.
- **The dispatcher is work-stealing**. Fast workers pick up more tests.
  You don't hand-balance anything.
- **Retry re-enqueues**. A failed test goes back on the shared queue —
  any worker can grab it, not just the one that failed.

## A single test, end to end

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
- `afterEach` runs even when the test body fails. That is how teardown
  stays reliable.
- Retry is separate from this loop — a failed test goes back into the
  dispatcher; the diagram plays out again, possibly on a different
  worker.

## Why the shape is this shape

- **One engine, many frontends.** Adding a new test style (a new macro,
  a new DSL, an MCP tool) doesn't fork the execution path. It translates
  into a `TestPlan` and lets the core handle the rest.
- **Rust owns the hot path.** Polling, actionability checks, selector
  compilation, CDP transport — all Rust. The TypeScript `expect` wrapper
  is a thin shim that issues one NAPI call per assertion; the retry loop
  stays inside Rust.
- **Per-worker browser, per-test context.** Launching a browser is the
  most expensive thing you can do. Creating a context is cheap. Amortize
  the first, refresh the second.
- **Dispatch via enum, not trait object.** Uniform API without the
  vtable cost.

For the file-level map, see the [workspace section in the root
README](https://github.com/salamaashoush/ferridriver#project-layout).
