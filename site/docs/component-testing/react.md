# React

```bash
npm install -D @ferridriver/test @ferridriver/ct-react react react-dom
```

```tsx
// counter.ct.tsx
import { test, expect } from '@ferridriver/test';
import Counter from './Counter';

test('increments', async ({ mount }) => {
  const component = await mount(<Counter initial={5} />);
  await expect(component.locator('#count')).toHaveText('5');
  await component.locator('#inc').click();
  await expect(component.locator('#count')).toHaveText('6');
});
```

```bash
npx @ferridriver/test ct --framework react counter.ct.tsx
```

Uses React 18+ `createRoot` on the browser side. Supports React 19.

## Hooks

```js
// .ferridriver-ct/index.ts (or your CT setup file)
window.__ferriBeforeMount = window.__ferriBeforeMount || [];
window.__ferriBeforeMount.push(async ({ Component, props, hooksConfig }) => {
  // wrap in a provider, set up MSW, etc.
});
```

## Exports

```js
import { registerSourcePath, frameworkName, vitePlugin } from '@ferridriver/ct-react';
```

- `registerSourcePath` — absolute path to `registerSource.mjs` (consumed by `ct-core`)
- `frameworkName` — `"react"`
- `vitePlugin` — async factory returning `@vitejs/plugin-react`
