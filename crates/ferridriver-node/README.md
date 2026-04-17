# @ferridriver/node

Node.js / Bun bindings for the ferridriver browser automation library. NAPI-RS native addon with a Playwright-compatible API.

Most users want the higher-level test runner at [`@ferridriver/test`](https://www.npmjs.com/package/@ferridriver/test). Pull this package in directly when you need the raw `Browser` / `Page` / `Locator` primitives.

## Install

```bash
npm install @ferridriver/node
# or
bun add @ferridriver/node
```

The right platform binary is pulled in via `optionalDependencies`:

| Platform | Package |
|---|---|
| macOS arm64 | `@ferridriver/node-darwin-arm64` |
| Linux x64 (glibc) | `@ferridriver/node-linux-x64-gnu` |
| Linux arm64 (glibc) | `@ferridriver/node-linux-arm64-gnu` |
| Windows x64 | `@ferridriver/node-win32-x64-msvc` |

On macOS, the tarball also ships `fd_webkit_host` — the Objective-C subprocess used by the WebKit backend.

## Usage

```ts
import { Browser } from '@ferridriver/node';

const browser = await Browser.launch();
const page = await browser.newPageWithUrl('https://example.com');

// Playwright-style locators
const heading = page.locator('h1');
console.log(await heading.textContent());

// Form interaction
await page.locator('#email').fill('user@test.com');
await page.locator('#password').fill('secret');
await page.locator('button[type=submit]').click();

// Wait for navigation
await page.waitForUrl('/dashboard');

// Events: page.on returns a numeric listener id; page.off(id) removes it
const listenerId = page.on('response', (data) => {
  console.log(`${data.status} ${data.url}`);
});
page.off(listenerId);

// Screenshot
const png = await page.screenshot({ fullPage: true });

await browser.close();
```

## Backends

```ts
// Default: CdpPipe (Chrome via fd 3/4 pipes — fastest)
const a = await Browser.launch();

// CDP over WebSocket
const b = await Browser.launch({ backend: 'cdp-raw' });

// Connect to a running Chrome
const c = await Browser.connect('ws://localhost:9222/devtools/browser/...');

// WebKit via WKWebView (macOS only — ships with the darwin-arm64 package)
const d = await Browser.launch({ backend: 'webkit' });

// Firefox via WebDriver BiDi
const e = await Browser.launch({ backend: 'bidi' });
```

## Public API surface

Classes exported from `index.d.ts`: `ApiRequestContext`, `ApiResponse`, `Browser`, `BrowserContext`, `Codegen`, `Frame`, `Keyboard`, `Locator`, `Mouse`, `Page`, `Route`, `StepHandle`, `TestFixtures`, `TestInfo`, `TestRunner`.

Helper functions: `findInstalledChromium()`, `findInstalledHeadlessShell()`, `getBrowserCacheDir()`, `installChromium()`, `installChromiumHeadlessShell()`, `installChromiumWithDeps()`, `installSystemDeps()`.

See [Page API section in the workspace README](../../README.md#page-api) for the full method list (navigation, selectors, actions, queries, screenshots, network, events, emulation, cookies/storage, input devices).

## Differences from Playwright

- `page.on()` returns a numeric listener id; use `page.off(id)` to remove.
- `goto()` accepts optional `{ waitUntil, timeout }` as the second argument.
- `locator.orLocator(other)` / `locator.andLocator(other)` instead of `.or()` / `.and()` (Rust keyword conflict).
- `evaluateAll()` on a `Locator` takes a JavaScript expression string with `elements` in scope.

## Building

```bash
bun install
bun run build        # release build for host target
bun run build:debug  # debug build
bun test             # run NAPI test suite
```

The release pipeline builds all four platform targets in parallel in GitHub Actions.

## Requirements

- Node.js 16+ or Bun 1.0+
- Chrome / Chromium (auto-detected, or install via `npx @ferridriver/test install`)
- macOS 11+ for the WebKit backend

## License

MIT OR Apache-2.0
