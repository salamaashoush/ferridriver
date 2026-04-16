# @ferridriver/ct-svelte

Ferridriver component testing adapter for [Svelte](https://svelte.dev/) (4 and 5).

## Install

```bash
npm install -D @ferridriver/ct-svelte @ferridriver/test svelte
```

## Usage

```typescript
// counter.ct.ts
import { test, expect } from '@ferridriver/test';

test('counter increments', async ({ page }) => {
  await page.locator('#inc').click();
  await expect(page.locator('#count')).toHaveText('1');
});
```

```bash
npx @ferridriver/test ct --framework svelte
```

## How it works

This adapter provides a `registerSource.mjs` that mounts Svelte components into a `#root` element. Supports both Svelte 4 (`new Component()`) and Svelte 5 (`mount()`) APIs. The Vite dev server handles compilation.

## License

MIT OR Apache-2.0
