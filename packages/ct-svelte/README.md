# @ferridriver/ct-svelte

Ferridriver component testing adapter for [Svelte](https://svelte.dev/) (Svelte 4 and 5). Provides the framework glue (Vite plugin + browser-side mount/update/unmount bindings). The test API itself (`test`, `expect`, `mount` fixture) is exported from `@ferridriver/test`.

## Install

```bash
npm install -D @ferridriver/test @ferridriver/ct-svelte svelte
# or
bun add -d @ferridriver/test @ferridriver/ct-svelte svelte
```

## Usage

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

## How it works

The adapter exposes `registerSourcePath`, `frameworkName`, and an async `vitePlugin` factory that returns `@sveltejs/vite-plugin-svelte`. The `ct` CLI loads these via `--framework svelte` and hands them to `createCtRunner` from `@ferridriver/ct-core`, which starts a Vite dev server at `http://localhost:3100`.

On the browser side, `registerSource.mjs` installs `window.__ferriMount` / `__ferriUpdate` / `__ferriUnmount`. It detects the Svelte major version at runtime and delegates to `new Component({ target, props })` (Svelte 4) or `mount(Component, { target, props })` (Svelte 5).

## Exports

```js
import { registerSourcePath, frameworkName, vitePlugin } from '@ferridriver/ct-svelte';

registerSourcePath  // absolute path to registerSource.mjs (consumed by ct-core)
frameworkName       // "svelte"
vitePlugin()        // async () => @sveltejs/vite-plugin-svelte's .svelte export
```

## License

MIT OR Apache-2.0
