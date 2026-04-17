# @ferridriver/ct-solid

Ferridriver component testing adapter for [Solid](https://www.solidjs.com/). Provides the framework glue (Vite plugin + browser-side mount/update/unmount bindings). The test API itself (`test`, `expect`, `mount` fixture) is exported from `@ferridriver/test`.

## Install

```bash
npm install -D @ferridriver/test @ferridriver/ct-solid solid-js
# or
bun add -d @ferridriver/test @ferridriver/ct-solid solid-js
```

## Usage

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

## How it works

The adapter exposes `registerSourcePath`, `frameworkName`, and an async `vitePlugin` factory that returns `vite-plugin-solid`. The `ct` CLI loads these via `--framework solid` and hands them to `createCtRunner` from `@ferridriver/ct-core`, which starts a Vite dev server at `http://localhost:3100`.

On the browser side, `registerSource.mjs` installs `window.__ferriMount` / `__ferriUpdate` / `__ferriUnmount` on top of Solid's `render()` / `dispose()` from `solid-js/web`.

## Exports

```js
import { registerSourcePath, frameworkName, vitePlugin } from '@ferridriver/ct-solid';

registerSourcePath  // absolute path to registerSource.mjs (consumed by ct-core)
frameworkName       // "solid"
vitePlugin()        // async () => vite-plugin-solid default export
```

## License

MIT OR Apache-2.0
