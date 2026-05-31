# @ferridriver/node

[![npm](https://img.shields.io/npm/v/@ferridriver/node.svg?logo=npm&color=c97b4a)](https://www.npmjs.com/package/@ferridriver/node)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-c97b4a.svg)](https://github.com/salamaashoush/ferridriver)

Node.js and Bun bindings for the ferridriver browser engine. NAPI-RS
native addon with a Playwright-shaped API: `BrowserType`, `Browser`,
`BrowserContext`, `Page`, `Frame`, `Locator`, `ElementHandle`, `Route`,
`Mouse`, `Keyboard`, `Touchscreen`.

This package is the **core browser binding**. For test running, BDD, or
the MCP server, install the `ferridriver` CLI separately
(`cargo install ferridriver-cli`).

## Install

```bash
npm install @ferridriver/node
# or
bun add @ferridriver/node
```

The right platform binary is pulled in via `optionalDependencies`:

| Platform           | Package                                |
|--------------------|----------------------------------------|
| macOS arm64        | `@ferridriver/node-darwin-arm64`       |
| Linux x64 (glibc)  | `@ferridriver/node-linux-x64-gnu`      |
| Linux arm64 (glibc)| `@ferridriver/node-linux-arm64-gnu`    |

## Usage

```ts
import { chromium } from '@ferridriver/node';

const browser = await chromium().launch();
const page = await browser.newPageWithUrl('https://example.com');

const heading = page.locator('h1');
console.log(await heading.textContent());

await page.locator('#email').fill('user@test.com');
await page.locator('#password').fill('secret');
await page.locator('button[type=submit]').click();
await page.waitForUrl('/dashboard');

// Events: page.on returns a numeric listener id; page.off(id) removes it.
const listenerId = page.on('response', (data) => {
  console.log(`${data.status} ${data.url}`);
});
page.off(listenerId);

const png = await page.screenshot({ fullPage: true });

await browser.close();
```

## Backends

```ts
import { chromium, firefox, webkit } from '@ferridriver/node';

// Default: CdpPipe (Chromium over fd 3/4 pipes)
const a = await chromium().launch();

// CDP over WebSocket (CdpRaw)
const b = await chromium({ transport: 'ws' }).launch();

// Attach to an already-running Chromium
const c = await chromium().connect('ws://localhost:9222/devtools/browser/...');

// Playwright WebKit (cross-platform — requires the Playwright WebKit binary)
const d = await webkit().launch();

// Firefox via WebDriver BiDi
const e = await firefox().launch();
```

The `webkit` backend needs the Playwright WebKit binary
(`npx playwright install webkit` once, or set `FERRIDRIVER_WEBKIT`).

## Public API surface

Classes exported from `index.d.ts`: `Browser`, `BrowserContext`,
`BrowserType`, `Codegen`, `ConsoleMessage`, `Dialog`, `Disposable`,
`Download`, `ElementHandle`, `FileChooser`, `Frame`, `FrameLocator`,
`HttpClient`, `HttpResponse`, `JsHandle`, `Keyboard`, `Locator`, `Mouse`,
`Page`, `Request`, `Response`, `Route`, `Touchscreen`, `Video`,
`WebError`, `WebSocket`.

Factory functions: `chromium(options?)`, `firefox()`, `webkit()` — each
returns a `BrowserType`.

Helper functions: `findInstalledChromium()`, `findInstalledFirefox()`,
`findInstalledHeadlessShell()`, `getBrowserCacheDir()`, `installChromium()`,
`installChromiumHeadlessShell()`, `installChromiumWithDeps()`,
`installFirefox()`, `installSystemDeps()`.

## Notable differences from Playwright

- `page.on(event, handler)` returns a numeric listener id; remove with
  `page.off(id)`. (Playwright returns the `Page` itself.)
- A browser is created via the `chromium()` / `firefox()` / `webkit()`
  factories — each returns a `BrowserType` whose `.launch()` /
  `.connect()` produce a `Browser`. There is no `Browser.launch()` static.
- `locator.orLocator(other)` / `locator.andLocator(other)` are aliases for
  `.or()` / `.and()` (all four are exported).
- `goto(url, options?)` accepts an optional `{ waitUntil, timeout }`
  argument as the second parameter; defaults to `load`.
- `evaluateAll()` on a `Locator` accepts either a JavaScript expression
  string (with `elements` in scope) or a function.

## Building from source

```bash
bun install
bun run build         # release for host target
bun run build:debug   # debug
bun test              # NAPI test suite
```

The release pipeline builds all platform targets in parallel in
GitHub Actions (macOS arm64, Linux x64 glibc, Linux arm64 glibc).

## Requirements

- Node.js 16+ or Bun 1.0+
- Chrome / Chromium (auto-detected, or run `ferridriver install chromium`)
- Playwright WebKit binary for the `webkit` backend

## License

MIT OR Apache-2.0
