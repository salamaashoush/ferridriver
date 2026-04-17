# @ferridriver/ct-vue

Ferridriver component testing adapter for [Vue 3](https://vuejs.org/). Provides the framework glue (Vite plugin + browser-side mount/update/unmount bindings). The test API itself (`test`, `expect`, `mount` fixture) is exported from `@ferridriver/test`.

## Install

```bash
npm install -D @ferridriver/test @ferridriver/ct-vue vue
# or
bun add -d @ferridriver/test @ferridriver/ct-vue vue
```

## Usage

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

## How it works

The adapter exposes `registerSourcePath`, `frameworkName`, and an async `vitePlugin` factory that returns `@vitejs/plugin-vue`. The `ct` CLI loads these via `--framework vue` and hands them to `createCtRunner` from `@ferridriver/ct-core`, which starts a Vite dev server at `http://localhost:3100`.

On the browser side, `registerSource.mjs` installs `window.__ferriMount` / `__ferriUpdate` / `__ferriUnmount`, which wrap Vue's `createApp(...).mount('#root')` and `unmount()`.

## Exports

```js
import { registerSourcePath, frameworkName, vitePlugin } from '@ferridriver/ct-vue';

registerSourcePath  // absolute path to registerSource.mjs (consumed by ct-core)
frameworkName       // "vue"
vitePlugin()        // async () => @vitejs/plugin-vue default export
```

## License

MIT OR Apache-2.0
