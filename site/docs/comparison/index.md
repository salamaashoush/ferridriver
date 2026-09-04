# Comparison

Honest comparison against the four browser-automation tools you're most
likely choosing between, and against the agent-facing tooling each of
them now ships. Snapshot: 2026-08-29, against `playwright` 1.62.1,
`@playwright/mcp` 0.0.79, `@playwright/cli` 0.1.18 and
`chrome-devtools-mcp` 1.8.0. Every count below was taken from those
packages rather than from their documentation.

## At a glance

| | **ferridriver** | Playwright | Puppeteer | Selenium | Cypress |
|---|---|---|---|---|---|
| **First language** | Rust | TypeScript / Node | Node | Java | JavaScript |
| **Other bindings** | Node / Bun via NAPI | Python, Java, .NET | none | Python, JS, .NET, Ruby, Go (W3C) | none |
| **Engine** | Rust core | Node core (TS) | Node core (TS) | per-language client | Browser-resident |
| **Protocols** | CDP pipe, CDP WS, BiDi, Playwright WebKit Inspector | CDP, WebKit Inspector, Playwright FF | CDP, WebDriver BiDi | W3C WebDriver, CDP | none (runs in-page) |
| **Browsers** | Chromium, Firefox, WebKit (PW build) | Chromium, Firefox, WebKit (PW build) | Chromium, Firefox | All major + niche | Chromium, Firefox, Edge |
| **Real Safari** | no (Playwright WebKit) | no (Playwright WebKit) | no | yes (`safaridriver`) | no |
| **Auto-wait** | yes (Rust core) | yes | manual `waitForSelector` | manual | yes |
| **Strict locators** | yes | yes | no | no | no |
| **Network mocking** | `route` / `unroute` / HAR replay (all backends) | `route` / HAR | `setRequestInterception` | proxy / extension | `cy.intercept` |
| **Trace viewer** | Playwright-compatible ZIP, viewer embedded in the binary | yes | no | no (Selenium 4 has BiDi traces) | yes |
| **BDD** | bundled (`ferridriver-bdd`, 146 steps) | community plugins | community plugins | yes (per language) | community plugins |
| **MCP server** | bundled (11 tools) | first-party `@playwright/mcp` (69 tools) | Google's `chrome-devtools-mcp` (58 tools), on `puppeteer-core` | no | no |
| **Agent CLI + skills** | no | `@playwright/cli` (9 skill references) | no | no | no |
| **Parallel workers** | per-process MPMC dispatch | per-process | per-script | grid | per-spec (single browser) |
| **Test framework included** | yes (`ferridriver-test`) | yes (`@playwright/test`) | no (use Jest etc.) | yes (per language) | yes |
| **CI artifacts** | screenshot, video, trace ZIP, JUnit, HTML | same | DIY | DIY | screenshot, video, runner UI |
| **API stability** | pre-1.0 | stable | stable | stable | stable |
| **License** | MIT OR Apache-2.0 | Apache-2.0 | Apache-2.0 | Apache-2.0 | MIT |

## Pick ferridriver when

- **Your team writes Rust.** Tests live in the same toolchain as the
  product code. No Node sidecar.
- **You want one binary.** MCP server, BDD runner, browser installer,
  script runner — `ferridriver`.
- **You want native JS / TS BDD steps without Node in the run path.**
  Rolldown bundles, QuickJS executes. `package.json` and `node_modules`
  optional.
- **You want an AI-driven browser without spinning up another stack.**
  The MCP server ships in the binary.

## Pick Playwright when

- **You're already on Node** and the team is comfortable with the JS
  ecosystem.
- **You need API stability today.** ferridriver is pre-1.0.
- **You need a richer language SDK matrix** (Python, .NET, Java first-class).
- **Codegen with an inspector window matters.** ferridriver has UI mode
  and serves its own trace viewer, but its codegen has no picker
  window.
- **You want the CLI + SKILLs shape for a coding agent.**
  `@playwright/cli` ships a skill with nine reference documents,
  covering named sessions, request mocking, tracing, video and test
  generation. There is no ferridriver equivalent; the MCP server is the
  only agent surface here.

## Pick Puppeteer when

- **You only need Chromium** and want the smallest possible dependency
  surface in a Node project.
- **You don't need a test framework wrapper** — you'll BYO Jest /
  Vitest / Mocha.

## Pick Selenium when

- **You need real Safari** (`safaridriver`), real Edge legacy, or
  obscure browser-vendor drivers.
- **You operate a grid** for distributed cross-browser, cross-OS runs.
- **You're standardizing on W3C WebDriver** for vendor neutrality.

## Pick Cypress when

- **Time-travel debugging in the UI runner** is your team's main
  workflow.
- **You're testing only Chromium-family browsers** and the in-browser
  execution model fits (no multi-tab, no cross-origin, single browser
  per spec).

## What ferridriver doesn't do (yet)

- **HAR recording** — `recordHar` is accepted on the context options bag
  and does nothing, on every backend, without saying so. HAR *replay*
  (`routeFromHAR`) does work, on all four backends. Replay is what the
  network-mocking row above refers to.
- **Codegen picker** — `ferridriver codegen` records interactions and
  emits a runnable script (default TypeScript), but has no inspector
  window like Playwright's codegen. The picker itself exists as an API
  (`page.pickLocator()`); it is not wired into the codegen command.
- **A CLI + skills bundle for coding agents** — the MCP server is the
  only agent-facing surface here. See below.

## Agent tooling

The interesting comparison has moved. All three of these now ship
something aimed at agents, and they have picked different shapes.

| | ferridriver | `@playwright/mcp` | `chrome-devtools-mcp` |
|---|---|---|---|
| Tools | 11 | 69 | 58 (3 in `--slim`) |
| Shape | one binary, no runtime | Node, delegates to `playwright` | Node, on `puppeteer-core` |
| Arbitrary code in the page | `run_script`, `evaluate` | `browser_evaluate`, `browser_run_code_unsafe` | `evaluate_script` |
| Performance analysis | `diagnostics` (`trace_start` / `trace_stop`): the same 19 DevTools insights, checked against the engine | no | `performance_start_trace`, `performance_analyze_insight` |
| Accessibility | `page.checkAccessibility()` from `run_script` (axe-core in the page) | no | inside `lighthouse_audit` |
| SEO and best-practices audits | `page.checkPageQuality()` from `run_script`: ten of Lighthouse's own, checked against Lighthouse | no | inside `lighthouse_audit` |
| Heap snapshots | `page.takeHeapSnapshot()` from `run_script`: capture, analyse and diff, checked against DevTools' own heap engine | no | 13 tools |
| Chrome extensions / PWA / WebMCP | no | no | yes |
| Extending the SERVER itself | `ferridriver_extensions`: add your own tools, reloadable without a restart | no | no |
| Test generation | `run_bdd`, `codegen` | via `@playwright/cli` skills | no |

Two things worth taking from that table rather than the counts.

**A large tool surface is a cost, not a feature.** Every tool's schema
is in the model's context on every turn. Microsoft's own README now
points coding agents at `@playwright/cli` with SKILLs *instead* of the
MCP server, for exactly this reason, and Google ships a `--slim` mode
that drops `chrome-devtools-mcp` from 58 tools to 3. ferridriver's 11
are deliberate: `run_script` takes a whole program with `page`,
`context` and `request` bound, so a loop or a conditional needs one
tool call rather than a tool per verb.

**Where the depth is differs.** `chrome-devtools-mcp` is the only one
of the three with PWA and extension installation and WebMCP; if that is
the job, use it. Its thirteen heap-snapshot tools are one capability
here rather than thirteen: `page.takeHeapSnapshot()` hands back a handle
carrying the same queries, and the analysis behind them is [checked
against DevTools' own heap engine node by
node](https://github.com/salamaashoush/ferridriver/blob/main/crates/ferridriver-heap/tests/differential.rs).
ferridriver's performance analysis covers the same 19 DevTools insights
and is
[checked against the real devtools-frontend engine on recorded
traces](https://github.com/salamaashoush/ferridriver/blob/main/scripts/perf-diff/README.md),
which is a claim the others do not make about theirs, and it runs
in-process rather than shelling out to a bundle.

## Performance

Rough order-of-magnitude only. Real numbers vary by hardware, network,
and what each suite actually does. These figures were not re-measured
for the snapshot above; everything else on this page was.

| | ferridriver | Playwright | Puppeteer |
|---|---|---|---|
| Per-action CDP RTT (cdp-pipe / Chromium) | ~0.7 ms | ~1.0 ms | ~1.0 ms |
| Browser launch (cold) | ~250 ms | ~400 ms | ~350 ms |
| Browser launch (warm, overlapped 4 workers) | ~120 ms | ~250 ms | n/a |
| Single test wall-time (navigate + click + assert) | ~80 ms | ~120 ms | ~100 ms |

The pipe transport, Rust polling, and overlapped worker launches add
up. A 200-test suite at 4 workers typically runs 2–3× faster than the
same suite on `@playwright/test`. The gap shrinks as tests do more
real work (server round-trips, DOM-heavy assertions) — those costs are
the same everywhere.
