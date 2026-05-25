# Migrating from Playwright

ferridriver's API is deliberately Playwright-shaped. The `Browser` /
`Page` / `Locator` / `Frame` / `BrowserContext` surface ports over almost
unchanged. Tests are written in Rust (`#[ferritest]` via
`ferridriver-test`) or as Gherkin features whose step bodies are Rust or
JavaScript / TypeScript, run by `ferridriver bdd`. From JavaScript,
`@ferridriver/node` provides the browser API itself.

## The browser API is unchanged

Selectors, `Page`, `Locator`, `Frame`, `BrowserContext`, and tracing
output that opens with `npx playwright show-trace` all work as you'd
expect.

```ts
// @ferridriver/node — the core browser binding
import { Browser } from '@ferridriver/node';

const browser = await Browser.launch();
const page = await browser.newPageWithUrl('https://app.example.com/login');
await page.getByLabel('Email').fill('user@example.com');
await page.getByRole('button', { name: 'Sign in' }).click();
```

## Imports and scopes

| Playwright                                | ferridriver |
|-------------------------------------------|-------------|
| `import { chromium } from 'playwright'`   | `import { Browser, chromium } from '@ferridriver/node'` |
| `import { test, expect } from '@playwright/test'` | Rust `#[ferritest]` + `ferridriver_test::expect`, **or** Gherkin steps run by `ferridriver bdd` |

Move test bodies to Rust `#[ferritest]`, or express them as Gherkin
features with JS / TS step bodies (`ferridriver bdd --steps`). The
browser calls inside don't change.

## `expect` matchers

Assertions live in the Rust core (`ferridriver-expect`, 38 matchers
with auto-retry on the Playwright polling schedule). From a Gherkin
suite, use the built-in assertion steps; from Rust, use
`expect(&locator).to_*`. See [the `expect` reference](/test-runner/expect).

## Locator method renames

Two methods renamed because `or` / `and` are Rust keywords:

| Playwright          | ferridriver           |
|---------------------|------------------------|
| `locator.or(other)` | `locator.orLocator(other)`  |
| `locator.and(other)`| `locator.andLocator(other)` |

This carries through the NAPI binding, so the TS surface uses the same
names.

## Events: numeric listener ids

Playwright's `page.on` returns the `Page` itself; remove with
`page.off(event, handler)`.

ferridriver returns a numeric id you pass to `page.off`:

```ts
const id = page.on('response', (data) => {
  console.log(`${data.status} ${data.url}`);
});
page.off(id);
```

## Backends and browser choice

| Playwright flag                          | ferridriver equivalent |
|------------------------------------------|------------------------|
| `--project=chromium`                     | `--browser chromium` (implies `--backend cdp-pipe`) |
| `--project=firefox`                      | `--browser firefox --backend bidi` |
| `--project=webkit`                       | `--browser webkit --backend webkit` |
| `chromium.launch({ channel: 'chrome' })` | `LaunchOptions { executable_path: Some("..."), .. }` |
| attach to a running browser              | `--backend cdp-raw` + `Browser::connect("ws://...")` |

Default is `cdp-pipe` (Chromium over fd pipes). On a protocol-level
issue porting tests, try `--backend cdp-raw` to switch to CDP over
WebSocket.

The WebKit backend uses Playwright's WebKit binary
(`npx playwright install webkit` or `FERRIDRIVER_WEBKIT`). It is
cross-platform (Linux, macOS, Windows).

Firefox uses WebDriver BiDi; Firefox must already be installed (no
bundled binary).

## Auto-waiting differences

ferridriver's pre-action actionability check verifies: **attached,
visible, and not `aria-disabled`**. Playwright additionally checks for
**position stability** (rect unchanged across animation frames) and
**receives events** (no other element covers the target).

In practice this affects ~1% of tests — animating elements clicked
mid-transition, or overlays that partially cover a button. Both checks
are on the roadmap. For now, `expect(&locator).to_be_visible().await?;
locator.click().await?;` is the reliable workaround.

## Porting checklist

1. Swap `playwright` imports for `@ferridriver/node` (browser code is unchanged).
2. Rename `.or()` → `.orLocator()`, `.and()` → `.andLocator()` on any locators that use them.
3. Move test bodies to Rust `#[ferritest]`, or to Gherkin features with JS / TS step bodies run by `ferridriver bdd`.
4. Replace web-first assertions with the Rust `expect` matchers or BDD assertion steps.
5. Add a `backend` to your project configs if you want anything other than Chromium-via-pipes.
