# @ferridriver/test

High-performance E2E, BDD, and component test runner with a Playwright-compatible API, powered by a Rust engine.

## Install

```bash
npm install @ferridriver/test
# or
bun add @ferridriver/test
```

## Usage

```typescript
import { test, expect } from '@ferridriver/test';

test('login flow', async ({ page }) => {
  await page.goto('https://app.example.com/login');
  await page.locator('#email').fill('user@example.com');
  await page.locator('button[type=submit]').click();
  await expect(page).toHaveURL(/dashboard/);
});
```

```bash
npx @ferridriver/test test tests/
```

## Features

- Playwright-compatible `test()`, `expect()`, `describe()`, `beforeAll/afterAll`
- Parallel workers with browser-per-worker isolation
- BDD/Gherkin support (`.feature` files + step definitions)
- Component testing with React, Vue, Svelte, Solid adapters
- Auto-retrying assertions (32 matchers)
- Visual and text snapshot testing
- Video recording, tracing, screenshots on failure
- `--grep`, `--tag`, `--shard`, `--last-failed` filtering

## Documentation

See the [ferridriver README](https://github.com/salamaashoush/ferridriver) for full documentation.

## License

MIT OR Apache-2.0
