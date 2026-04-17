# Migrating from Playwright

ferridriver's API is deliberately Playwright-shaped. Most tests port over with a find-and-replace on the import statement. This page calls out the real differences — what's renamed, what's missing, and what's different in behavior.

## The 80% that is unchanged

Selectors, `Page`, `Locator`, `Frame`, `BrowserContext`, fixtures injected into the test, `expect` with auto-retry, `test.describe`, `.spec.ts` file conventions, tracing output that opens with `npx playwright show-trace` — all work as you'd expect.

```ts
// This is valid in both Playwright and @ferridriver/test.
import { test, expect } from '@ferridriver/test';

test('login', async ({ page }) => {
  await page.goto('https://app.example.com/login');
  await page.getByLabel('Email').fill('user@example.com');
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page).toHaveURL('https://app.example.com/dashboard');
});
```

Below are the places where you'll need to change something.

## Imports and scopes

| Playwright | ferridriver |
|---|---|
| `import { test, expect } from '@playwright/test'` | `import { test, expect } from '@ferridriver/test'` |
| `import { chromium } from 'playwright'` | `import { Browser } from '@ferridriver/node'` |
| `import { test, expect } from '@playwright/experimental-ct-react'` | `import { test, expect } from '@ferridriver/test'` + `@ferridriver/ct-react` as the Vite adapter |

For components, `test` and `expect` come from `@ferridriver/test` regardless of framework. The adapter package only provides `registerSourcePath`, `frameworkName`, and `vitePlugin` — the test API is shared.

## `expect` matchers: smaller surface on the TypeScript side

Playwright's web-first assertions expose ~30 matchers on the TypeScript side. ferridriver currently exposes **13** in TS; the Rust core has 38 and is the source of truth. The rest land incrementally.

Today's TS matcher set: `toHaveTitle`, `toHaveURL`, `toBeVisible`, `toBeHidden`, `toBeEnabled`, `toBeDisabled`, `toBeChecked`, `toHaveText`, `toContainText`, `toHaveValue`, `toHaveAttribute`, `toHaveCount`, `toPass`.

**No regex arguments on the TS side yet.** `toHaveURL(/dashboard/)` works in Playwright; today use `toHaveURL('https://app.example.com/dashboard')`. Regex is supported in Rust via `impl Into<StringOrRegex>`.

For matchers not yet in the TS surface (e.g. `toBeEditable`, `toHaveClass`, `toMatchAriaSnapshot`), the escape hatch is `toPass`:

```ts
await expect(async () => {
  const count = await page.locator('.row').count();
  if (count < 10) throw new Error(`expected 10+, got ${count}`);
}).toPass();
```

See [`expect` reference](/test-runner/expect) for the complete table.

## Locator method renames (Rust + TypeScript)

Two methods were renamed because `or` / `and` are Rust keywords:

| Playwright | ferridriver |
|---|---|
| `locator.or(other)` | `locator.orLocator(other)` |
| `locator.and(other)` | `locator.andLocator(other)` |

The TypeScript side follows the same rename to keep a single surface.

## Events: numeric listener ids

Playwright's `page.on` returns the Page itself; you remove with `page.off(event, handler)`.

ferridriver returns a numeric id you pass to `page.off`:

```ts
const id = page.on('response', (data) => {
  console.log(`${data.status} ${data.url}`);
});
page.off(id);
```

## `test.beforeAll` / `test.afterAll`

Playwright attaches hooks to the `test` namespace. ferridriver's hook macros (Rust) and named exports (TS) are separate identifiers:

| Playwright | ferridriver TS | ferridriver Rust |
|---|---|---|
| `test.beforeAll(async () => ...)` | `BeforeAll(async () => ...)` | `#[before_all] async fn setup(ctx: TestContext) { }` |
| `test.afterAll` | `AfterAll` | `#[after_all]` |
| `test.beforeEach` | `BeforeStep` (and `Before` for BDD) | `#[before_each]` |
| `test.afterEach` | `AfterStep` (and `After` for BDD) | `#[after_each]` |

In Rust, every hook receives a `TestContext` regardless of kind (see [Fixtures and hooks](/test-runner/fixtures-and-hooks)).

## Fixtures

Playwright's custom-fixture `test.extend({ ... })` pattern doesn't have a direct TS equivalent in ferridriver today. The Rust runner has a fully typed, DAG-validated fixture system ([Fixtures](/concepts/fixtures)); the TS surface currently exposes only the built-ins (`page`, `context`, `browser`, `mount`, `testInfo`).

Workaround on the TS side: plain helper functions that take `page` and return what you need. For anything worker-scoped, prefer a Rust-side custom fixture.

## Backends and browser choice

| Playwright flag | ferridriver equivalent |
|---|---|
| `--project=chromium` | `--browser chromium` (implies `--backend cdp-pipe`) |
| `--project=firefox` | `--browser firefox --backend bidi` |
| `--project=webkit` | `--browser webkit --backend webkit` (macOS only) |
| `chromium.launch({ channel: 'chrome' })` | `LaunchOptions { executable_path: Some("...") }` |
| attach to a running browser | `--backend cdp-raw` + `Browser.connect("ws://...")` |

ferridriver's default is **`cdp-pipe`**, which Playwright doesn't expose at all (Playwright always uses WebSocket). If you hit a protocol-level issue porting tests, try `--backend cdp-raw` — it's the same transport as Playwright.

WebKit in ferridriver is **native macOS WKWebView** (via a bundled `fd_webkit_host` subprocess), not the patched WebKit build Playwright ships. That means:
- No Windows / Linux WebKit support.
- The accessibility tree is the real AX tree macOS exposes, which is often *more* accurate than Playwright's WebKit.
- Some CDP-only features (e.g. CDP tracing) aren't available on this backend.

Firefox in ferridriver uses **WebDriver BiDi**, not Playwright's Juggler patch. Firefox must already be installed; ferridriver does not bundle it.

## Auto-waiting differences

ferridriver's pre-action actionability check verifies: attached, visible, and not `aria-disabled`. Playwright additionally checks for **position stability** (rect unchanged across animation frames) and **receives events** (no other element covers the target).

In practice this affects ~1% of tests — animating elements that are clicked mid-transition, or overlays that partially cover a button. Workaround:

```ts
// assert visibility (which waits past the transition), then act
await expect(locator).toBeVisible();
await locator.click();
```

Both stability and receives-events checks are on the roadmap.

## Missing today (targets for parity)

Honest list, roughly in decreasing impact:

- Trace viewer UI (format is Playwright-compatible — use `npx playwright show-trace`)
- Playwright Inspector equivalent (live-step debugger)
- `test.extend({ })` for custom TS fixtures
- Regex arguments to TS `expect` matchers
- Stability / receives-events actionability checks
- `page.route` glob patterns beyond the basics
- Video recording options (`size`, `dir`) — only the on/off/retain-on-failure modes today
- Some rarely used `Locator` methods (`evaluateHandle`, `elementHandle` — deliberately not exposed; use `evaluate`)

Features ferridriver has that Playwright doesn't:

- **Mixed `.feature` + `.spec.ts` in one run**, sharing workers and reporters.
- **First-class MCP server** for AI-driven automation.
- **Native macOS WebKit** — Playwright's is a patched fork.
- **BDD framework** (`ferridriver-bdd`) with 144 built-in Gherkin steps backed by the same engine, not a separate layer.
- **CDP Pipe transport** — measurable per-action latency win over WebSocket.

## Porting checklist

1. Replace imports.
2. Rename `or` → `orLocator`, `and` → `andLocator` on any locators that use them.
3. Replace `test.beforeAll(...)` etc. with the named TS exports or the Rust hook macros.
4. Convert any regex `toHaveURL` / `toHaveTitle` calls to strings.
5. Swap `test.extend({ })` custom fixtures for helper functions (or move the fixture to Rust).
6. Add a `backend` to your project configs if you want anything other than Chromium-via-pipes.
7. Run once with `--backend cdp-raw` if anything behaves oddly — that's Playwright's transport model.
