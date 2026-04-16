# @ferridriver/ct-solid

Ferridriver component testing adapter for [Solid](https://www.solidjs.com/).

## Install

```bash
npm install -D @ferridriver/ct-solid @ferridriver/test solid-js
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
npx @ferridriver/test ct --framework solid
```

## How it works

This adapter provides a `registerSource.mjs` that renders Solid components into a `#root` element using `solid-js/web`'s `render()`. The Vite dev server handles compilation.

## License

MIT OR Apache-2.0
