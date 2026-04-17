# @ferridriver/ct-core

Core infrastructure for JavaScript framework component testing. Handles the pipeline from test file scanning to component mounting in a real browser.

Most users don't depend on this package directly — it's pulled in transitively through `@ferridriver/test` and the framework adapters (`@ferridriver/ct-react`, `ct-vue`, `ct-svelte`, `ct-solid`). Use it directly only when embedding the CT pipeline into a custom runner.

## Install

```bash
npm install -D @ferridriver/ct-core vite
# or
bun add -d @ferridriver/ct-core vite
```

`vite >= 5.0.0` is a peer dependency.

## Architecture

```
Test files          Vite build          Browser
----------          ----------          -------
import Counter  --> importTransform     ImportRegistry
  from './C'        rewrites to           resolves lazy
                    importRef             import() calls
                         |                     |
                    vitePlugin              unwrapObject
                    injects registry        resolves refs
                    + runtime                  |
                         |               __ferriMount()
                    Vite bundle          (framework-specific)
                         |                     |
                    dev server  <------  page.evaluate()
```

### Pipeline

1. **Import transform** -- Scans test files, rewrites component imports to `importRef` descriptors
2. **Vite plugin** -- Injects browser runtime + framework registerSource + lazy `import()` for each component
3. **Dev server** -- Vite serves the bundle at `http://localhost:3100`
4. **mount()** -- Serializes JSX tree (replacing functions with ordinal refs), sends to browser via `page.evaluate()`
5. **Browser runtime** -- `ImportRegistry` resolves component refs, `unwrapObject` resolves function refs, framework's `__ferriMount()` renders

## Import Transform

```javascript
// BEFORE (test file):
import Counter from './Counter';
import { Button } from '../components/Button';

// AFTER (transformed):
const Counter = { __pw_type: 'importRef', id: '_src_Counter' };
const Button = { __pw_type: 'importRef', id: '_components_Button', property: 'Button' };
```

The registry maps each ID to a lazy `import()`:
```javascript
const _src_Counter = () => import('/abs/path/Counter').then(mod => mod.default);
```

## Browser Runtime (`injected/index.js`)

Installed as globals on `window`:

| Global | Purpose |
|---|---|
| `__ferriRegistry` | `ImportRegistry` -- maps component IDs to lazy imports |
| `__ferriUnwrapObject` | Recursively resolves `importRef` and `function` refs |
| `__ferriMount` | Set by framework registerSource (e.g., ct-react) |
| `__ferriUpdate` | Re-render with new props |
| `__ferriUnmount` | Tear down mounted component |
| `__ferriDispatchFunction` | Callback bridge for event handlers |

## mount.mjs API

```javascript
import { mount, unmount, update, wrapObject, createComponent } from '@ferridriver/ct-core';

// Mount a component (called from test fixtures)
const locator = await mount(page, componentRef, { props: { count: 5 } }, boundCallbacks);
// Returns a Locator pointing at #root

// Update props
await update(page, { props: { count: 10 } }, boundCallbacks);

// Unmount
await unmount(page);
```

`wrapObject` replaces JS functions with `{ __pw_type: 'function', ordinal: N }` for serialization across the Node-to-browser boundary.

## Vite Plugin

```javascript
import { ferridriverCtPlugin } from '@ferridriver/ct-core';

// componentRegistry: Map<string, { importSource, remoteName }>
const plugin = ferridriverCtPlugin(componentRegistry, registerSourcePath);
```

The plugin transforms the `.ferridriver-ct/index.ts` entry file to inject:
- The browser runtime (ImportRegistry, unwrapObject)
- The framework registerSource
- Lazy `import()` for every component in the registry
- `window.__ferriRegistry.initialize({ ... })`

## createCtRunner

```javascript
import { createCtRunner } from '@ferridriver/ct-core';

const runner = await createCtRunner({
  projectDir: process.cwd(),
  testFiles: ['/abs/path/to/test.ct.tsx'],
  registerSourcePath: '/path/to/ct-react/registerSource.mjs',
  frameworkPlugin: () => import('@vitejs/plugin-react').then(m => m.default()),
  port: 3100,
});

console.log(runner.baseUrl);  // http://127.0.0.1:3100/__ferri_ct_index.html
await runner.stop();
```

## Exports

Top-level (`@ferridriver/ct-core`):

```js
export { mount, unmount, update, wrapObject, createComponent } from './mount.mjs';
export { createCtRunner } from './runner.mjs';
export { ferridriverCtPlugin } from './vitePlugin.mjs';
export { transformTestFile, scanTestFiles } from './importTransform.mjs';
```

Subpath exports (for deeper integration):

| Subpath | Purpose |
|---|---|
| `@ferridriver/ct-core/mount` | `mount` / `unmount` / `update` and serializer helpers |
| `@ferridriver/ct-core/runner` | `createCtRunner` (starts the Vite dev server) |
| `@ferridriver/ct-core/vitePlugin` | `ferridriverCtPlugin` (the Vite plugin factory) |
| `@ferridriver/ct-core/importTransform` | `transformTestFile`, `scanTestFiles` |
| `@ferridriver/ct-core/jsx-runtime` | JSX pragma for test files |
| `@ferridriver/ct-core/injected` | Browser-side runtime (`ImportRegistry`, `unwrapObject`) |

## License

MIT OR Apache-2.0
