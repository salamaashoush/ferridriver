# Architecture

ferridriver is a workspace of Rust crates plus a set of TypeScript packages built on top of them. Everything above the core dispatches to one engine.

## Layers

```
┌─────────────────────────────────────────────────────────────┐
│  @ferridriver/test   @ferridriver/ct-{react,vue,svelte,solid}│  TS
├─────────────────────────────────────────────────────────────┤
│  @ferridriver/node                          (NAPI binding)   │
├─────────────────────────────────────────────────────────────┤
│  ferridriver-cli (MCP)   ferridriver-test   ferridriver-bdd  │  Rust
│                                                              │
│  ferridriver-mcp (28 tools)   test-macros   bdd-macros       │
├─────────────────────────────────────────────────────────────┤
│  ferridriver (Browser, Page, Locator, Frame, Context)        │
├──────────────┬─────────────────┬────────────────┬───────────┤
│   CdpPipe    │     CdpRaw      │     WebKit     │   Bidi    │  Backends
│  (Chrome     │   (Chrome via   │  (WKWebView    │ (Firefox  │
│   pipes)     │    WebSocket)   │    on macOS)   │   BiDi)   │
└──────────────┴─────────────────┴────────────────┴───────────┘
```

## Backends

Backends are dispatched as an `enum`, not trait objects. This keeps cross-backend calls monomorphic and lets the compiler inline per-backend paths.

| Backend | Transport | When to use |
|---|---|---|
| **CdpPipe** | Chrome via fd 3/4 Unix pipes | Default, lowest latency |
| **CdpRaw** | Chrome DevTools Protocol over WebSocket | Attach to a running browser, or run fully parallel |
| **WebKit** | Objective-C subprocess IPC to WKWebView | Native macOS testing, native accessibility tree |
| **Bidi** | WebDriver BiDi over WebSocket | Firefox support |

## Test execution flow

All test types (E2E, BDD, component, NAPI) share one execution pipeline in `ferridriver-test::TestRunner::run()`:

```
TestPlan
  → filter (shard, grep, tag, only, last-failed)
  → validate fixture DAG
  → run global setup
  → Dispatcher (MPMC work-stealing)
      → W0  W1  W2  W3   each with its own Browser
  → collect + retry failed tests
  → run global teardown
```

BDD translates `.feature` files into the same `TestPlan` — the BDD crate is a translation layer, not a parallel runner.

## Packages

See the [workspace README on GitHub](https://github.com/salamaashoush/ferridriver#workspace) for the full crate and package list.
