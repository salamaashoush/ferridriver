# @ferridriver/node

[![npm](https://img.shields.io/npm/v/@ferridriver/node.svg?logo=npm&color=c97b4a)](https://www.npmjs.com/package/@ferridriver/node)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-c97b4a.svg)](https://github.com/salamaashoush/ferridriver)

Node.js and Bun bindings for the ferridriver browser engine. NAPI-RS
native addon with a Playwright-shaped API: `Browser`, `BrowserContext`,
`Page`, `Frame`, `Locator`, `ElementHandle`, `Route`, `Mouse`,
`Keyboard`, `Touchscreen`.

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
| Windows x64        | `@ferridriver/node-win32-x64-msvc`     |

## Usage

```ts
import { Browser } from '@ferridriver/node';

const browser = await Browser.launch();
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
// Default: CdpPipe (Chromium over fd 3/4 pipes — fastest)
const a = await Browser.launch();

// CDP over WebSocket
const b = await Browser.launch({ backend: 'cdp-raw' });

// Attach to an already-running Chromium
const c = await Browser.connect('ws://localhost:9222/devtools/browser/...');

// Playwright WebKit (cross-platform — requires the Playwright WebKit binary)
const d = await Browser.launch({ backend: 'webkit' });

// Firefox via WebDriver BiDi
const e = await Browser.launch({ backend: 'bidi' });
```

The `webkit` backend needs the Playwright WebKit binary
(`npx playwright install webkit` once, or set `FERRIDRIVER_WEBKIT`).

## Public API surface

Classes exported from `index.d.ts`: `ApiRequestContext`, `ApiResponse`,
`Browser`, `BrowserContext`, `Codegen`, `Frame`, `Keyboard`, `Locator`,
`Mouse`, `Page`, `Route`, `StepHandle`, `TestFixtures`, `TestInfo`,
`TestRunner`, `Touchscreen`.

Helper functions: `findInstalledChromium()`, `findInstalledHeadlessShell()`,
`getBrowserCacheDir()`, `installChromium()`, `installChromiumHeadlessShell()`,
`installChromiumWithDeps()`, `installSystemDeps()`.

## Notable differences from Playwright

- `page.on(event, handler)` returns a numeric listener id; remove with
  `page.off(id)`. (Playwright returns the `Page` itself.)
- `locator.orLocator(other)` / `locator.andLocator(other)` instead of
  `.or()` / `.and()` — `or` / `and` are Rust keywords and the rename
  carries through NAPI.
- `goto(url, options?)` accepts an optional `{ waitUntil, timeout }`
  argument as the second parameter; defaults to `load`.
- `evaluateAll()` on a `Locator` takes a JavaScript expression string with
  `elements` in scope, not a function.

## Building from source

```bash
bun install
bun run build         # release for host target
bun run build:debug   # debug
bun test              # NAPI test suite
```

The release pipeline builds all four platform targets in parallel in
GitHub Actions.

## Requirements

- Node.js 18+ or Bun 1.0+
- Chrome / Chromium (auto-detected, or run `ferridriver install chromium`)
- Playwright WebKit binary for the `webkit` backend

## License

MIT OR Apache-2.0
