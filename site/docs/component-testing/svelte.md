# Svelte

Supports Svelte 4 and Svelte 5.

```bash
npm install -D @ferridriver/test @ferridriver/ct-svelte svelte
```

```ts
// counter.ct.ts
import { test, expect } from '@ferridriver/test';
import Counter from './Counter.svelte';

test('increments', async ({ mount }) => {
  const component = await mount(Counter, { props: { initial: 5 } });
  await expect(component.locator('#count')).toHaveText('5');
  await component.locator('#inc').click();
  await expect(component.locator('#count')).toHaveText('6');
});
```

```bash
npx @ferridriver/test ct --framework svelte counter.ct.ts
```

The adapter detects the Svelte major version at runtime and delegates to `new Component({ target, props })` (Svelte 4) or `mount(Component, { target, props })` (Svelte 5).

## Exports

```js
import { registerSourcePath, frameworkName, vitePlugin } from '@ferridriver/ct-svelte';
```

`vitePlugin` returns the `.svelte` export of `@sveltejs/vite-plugin-svelte`.
