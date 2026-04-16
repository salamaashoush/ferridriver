# @ferridriver/ct-vue

Ferridriver component testing adapter for [Vue 3](https://vuejs.org/).

## Install

```bash
npm install -D @ferridriver/ct-vue @ferridriver/test vue
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
npx @ferridriver/test ct --framework vue
```

## How it works

This adapter provides a `registerSource.mjs` that mounts Vue components into a `#root` element using `createApp()` and `mount()`. The Vite dev server handles compilation.

## License

MIT OR Apache-2.0
