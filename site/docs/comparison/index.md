# Comparison

Honest comparison against the four browser-automation tools you're most
likely choosing between. Snapshot: 2026-05-25.

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
| **Network mocking** | `route` / `unroute` / HAR (no BiDi) | `route` / HAR | `setRequestInterception` | proxy / extension | `cy.intercept` |
| **Trace viewer** | Playwright-compatible ZIP | yes | no | no (Selenium 4 has BiDi traces) | yes |
| **BDD** | bundled (`ferridriver-bdd`, 145 steps) | community plugins | community plugins | yes (per language) | community plugins |
| **MCP server** | bundled (10 tools) | community | community | no | no |
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
- **Codegen, UI mode, and the full Trace Viewer integration matter.**
  ferridriver produces Playwright-compatible traces but does not ship
  the UI itself.

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

- **Position stability** check before clicks — Playwright re-hits coords
  to ensure nothing is mid-animation. On the roadmap.
- **Receives-events** check before clicks — Playwright re-hits coords to
  ensure nothing covers the target. On the roadmap.
- **UI mode** — no time-travel debugger. Use the Playwright trace
  viewer on the trace ZIP we emit.
- **Codegen** — `ferridriver codegen` records interactions and emits a
  runnable script (default TypeScript), but has no interactive picker /
  inspector UI like Playwright's codegen window.
- **HAR on BiDi backend** — returns `Unsupported`. Use a CDP backend.
- **Download events on BiDi backend** — returns `Unsupported`. Use a
  CDP backend.

## Performance

Rough order-of-magnitude only. Real numbers vary by hardware, network,
and what each suite actually does.

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
