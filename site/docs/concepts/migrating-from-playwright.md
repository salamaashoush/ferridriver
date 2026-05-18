# Migrating from Playwright

ferridriver's API is deliberately Playwright-shaped. The `Browser` / `Page`
/ `Locator` surface ports over almost unchanged. Tests are written in Rust
(`#[ferritest]` via `ferridriver-test`) or as Gherkin features whose step
bodies are Rust or JavaScript/TypeScript, run by `ferridriver bdd`. From
JavaScript, `@ferridriver/node` provides the browser API itself.

## The browser API is unchanged

Selectors, `Page`, `Locator`, `Frame`, `BrowserContext`, and tracing output
that opens with `npx playwright show-trace` all work as you'd expect.

```ts
// @ferridriver/node — the core browser binding.
import { Browser } from '@ferridriver/node';

const browser = await Browser.launch();
const page = await browser.newPageWithUrl('https://app.example.com/login');
await page.getByLabel('Email').fill('user@example.com');
await page.getByRole('button', { name: 'Sign in' }).click();
```

## Imports and scopes

| Playwright | ferridriver |
|---|---|
| `import { chromium } from 'playwright'` | `import { Browser, chromium } from '@ferridriver/node'` |
| `import { test, expect } from '@playwright/test'` | Rust `#[ferritest]` + `ferridriver_test::expect`, **or** Gherkin steps run by `ferridriver bdd` |

Move test bodies to Rust `#[ferritest]`, or express them as Gherkin
features with JavaScript/TypeScript step bodies (`ferridriver bdd
--steps`). The browser calls inside don't change.

## `expect` matchers

Assertions live in the Rust core (`ferridriver-test::expect`, 38 matchers
with auto-retry). From a Gherkin suite, use the built-in assertion steps;
from Rust, use `expect(&locator).to_*`. See the
[`expect` reference](/test-runner/expect).

## Locator method renames

Two methods were renamed because `or` / `and` are Rust keywords:

| Playwright | ferridriver |
|---|---|
| `locator.or(other)` | `locator.orLocator(other)` |
| `locator.and(other)` | `locator.andLocator(other)` |

## Events: numeric listener ids

Playwright's `page.on` returns the Page itself; you remove with `page.off(event, handler)`.

ferridriver returns a numeric id you pass to `page.off`:

```ts
const id = page.on('response', (data) => {
  console.log(`${data.status} ${data.url}`);
});
page.off(id);
```

## Backends and browser choice

| Playwright flag | ferridriver equivalent |
|---|---|
| `--project=chromium` | `--browser chromium` (implies `--backend cdp-pipe`) |
| `--project=firefox` | `--browser firefox --backend bidi` |
| `--project=webkit` | `--browser webkit --backend webkit` (macOS only) |
| `chromium.launch({ channel: 'chrome' })` | `LaunchOptions { executable_path: Some("...") }` |
| attach to a running browser | `--backend cdp-raw` + `Browser.connect("ws://...")` |

ferridriver's default is **`cdp-pipe`** (CDP over file-descriptor pipes). If you hit a protocol-level issue porting tests, try `--backend cdp-raw`, which uses a CDP WebSocket.

WebKit in ferridriver is **native macOS WKWebView** (via a bundled `fd_webkit_host` subprocess). That means:
- No Windows / Linux WebKit support.
- The accessibility tree is the real AX tree macOS exposes.
- Some CDP-only features (e.g. CDP tracing) aren't available on this backend.

Firefox in ferridriver uses **WebDriver BiDi**. Firefox must already be installed; ferridriver does not bundle it.

## Auto-waiting differences

ferridriver's pre-action actionability check verifies: attached, visible, and not `aria-disabled`. Playwright additionally checks for **position stability** (rect unchanged across animation frames) and **receives events** (no other element covers the target).

In practice this affects ~1% of tests — animating elements that are clicked mid-transition, or overlays that partially cover a button. Both stability and receives-events checks are on the roadmap.

## Porting checklist

1. Swap `playwright` imports for `@ferridriver/node` (browser code is unchanged).
2. Rename `or` → `orLocator`, `and` → `andLocator` on any locators that use them.
3. Move test bodies to Rust `#[ferritest]`, or to Gherkin features with JS/TS step bodies run by `ferridriver bdd`.
4. Replace web-first assertions with the Rust `expect` matchers / BDD assertion steps.
5. Add a `backend` to your project configs if you want anything other than Chromium-via-pipes.
