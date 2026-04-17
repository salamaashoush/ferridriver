# Vue

```bash
npm install -D @ferridriver/test @ferridriver/ct-vue vue
```

```ts
// counter.ct.ts
import { test, expect } from '@ferridriver/test';
import Counter from './Counter.vue';

test('increments', async ({ mount }) => {
  const component = await mount(Counter, { props: { initial: 5 } });
  await expect(component.locator('#count')).toHaveText('5');
  await component.locator('#inc').click();
  await expect(component.locator('#count')).toHaveText('6');
});
```

```bash
npx @ferridriver/test ct --framework vue counter.ct.ts
```

On the browser side, `registerSource.mjs` wraps Vue's `createApp(...).mount('#root')` and `unmount()`.

## Exports

```js
import { registerSourcePath, frameworkName, vitePlugin } from '@ferridriver/ct-vue';
```

`vitePlugin` returns `@vitejs/plugin-vue`.
