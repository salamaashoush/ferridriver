# Solid

```bash
npm install -D @ferridriver/test @ferridriver/ct-solid solid-js
```

```tsx
// counter.ct.tsx
import { test, expect } from '@ferridriver/test';
import Counter from './Counter';

test('increments', async ({ mount }) => {
  const component = await mount(() => <Counter initial={5} />);
  await expect(component.locator('#count')).toHaveText('5');
  await component.locator('#inc').click();
  await expect(component.locator('#count')).toHaveText('6');
});
```

```bash
npx @ferridriver/test ct --framework solid counter.ct.tsx
```

On the browser side, `registerSource.mjs` uses Solid's `render()` / `dispose()` from `solid-js/web`.

## Exports

```js
import { registerSourcePath, frameworkName, vitePlugin } from '@ferridriver/ct-solid';
```

`vitePlugin` returns `vite-plugin-solid`.
