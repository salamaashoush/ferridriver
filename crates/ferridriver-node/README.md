# ferridriver-node

Node.js/Bun bindings for the ferridriver browser automation library. NAPI-RS native addon.

## Installation

```bash
bun add ferridriver-node
# or
npm install ferridriver-node
```

## Usage

```typescript
import { Browser } from 'ferridriver-node';

const browser = await Browser.launch();
const page = await browser.page();

await page.goto('https://example.com');

// Playwright-style locators
const heading = page.locator('h1');
console.log(await heading.textContent());

// Form interaction
await page.locator('#email').fill('user@test.com');
await page.locator('#password').fill('secret');
await page.locator('button[type=submit]').click();

// Wait for navigation
await page.waitForUrl('/dashboard');

// Events
const listenerId = page.on('response', (data) => {
  console.log(`${data.status} ${data.url}`);
});
page.off(listenerId);

// Screenshot
const png = await page.screenshot({ fullPage: true });

// Accessibility snapshot
const state = await page.storageState();

await browser.close();
```

## Backends

```typescript
// Default: CdpPipe (fastest)
const browser = await Browser.launch();

// WebSocket (connect to running Chrome)
const browser = await Browser.launch({ backend: 'cdp-raw' });

// Connect to existing browser
const browser = await Browser.connect('ws://localhost:9222/...');

// WebKit (macOS only)
const browser = await Browser.launch({ backend: 'webkit' });
```

## Building

```bash
bun run build       # Release build
bun run build:debug # Debug build
bun test            # Run tests (250 tests across 3 backends)
```

## API

See the [workspace README](../../README.md) for the complete API reference. The NAPI bindings expose the same API as the Rust library with JavaScript-native types.

### Key differences from Playwright

- `page.on()` returns a numeric listener ID (use `page.off(id)` to remove)
- `goto()` accepts optional `{ waitUntil, timeout }` as second argument
- `locator.orLocator(other)` / `locator.andLocator(other)` instead of `locator.or(other)` / `locator.and(other)` (Rust keyword conflict)
- `evaluateAll()` on Locator takes a JS expression string with `elements` in scope

## Requirements

- Node.js 18+ or Bun 1.0+
- Chrome/Chromium (auto-detected)
- macOS 11+ for WebKit backend
