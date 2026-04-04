# @ferridriver/ct-react

React component testing adapter for ferridriver. Mount React components in a real browser, interact with them using the full Page/Locator API, and assert with auto-retrying matchers.

## Usage

```typescript
import { test, expect } from '@ferridriver/ct-react';
import Counter from './Counter';

test('increments', async ({ mount, page }) => {
  const component = await mount(<Counter initial={5} />);
  await expect(component.locator('#count')).toHaveText('5');
  await component.locator('#inc').click();
  await expect(component.locator('#count')).toHaveText('6');
});

test('renders with props', async ({ mount }) => {
  const component = await mount(<Counter initial={100} />);
  await expect(component.locator('#count')).toHaveText('100');
});

test.describe('reset button', () => {
  test('resets to zero', async ({ mount }) => {
    const component = await mount(<Counter initial={5} />);
    await component.locator('#inc').click();
    await component.locator('#reset').click();
    await expect(component.locator('#count')).toHaveText('0');
  });
});
```

## How It Works

### Test Side (Node/Bun)

The `test()` function:
1. Launches a browser (once, shared across tests in a describe block)
2. Creates a fresh page per test, navigated to the CT preview server
3. Provides `mount` and `page` fixtures to the test body

`mount(component, options)` calls `ct-core/mount.mjs` which:
- Serializes the JSX tree (functions become ordinal refs)
- Sends it to the browser via `page.evaluate()`
- Returns a locator scoped to `#root`

### Browser Side (registerSource.mjs)

Injected into the Vite bundle, defines three globals:

```javascript
window.__ferriMount = async (componentRef, rootElement, options) => {
  // Resolves component from ImportRegistry
  // Runs beforeMount hooks
  // createRoot(rootElement).render(<Component {...props} />)
  // Runs afterMount hooks
};

window.__ferriUpdate = (rootElement, newProps) => {
  // Re-renders with new props via root.render()
};

window.__ferriUnmount = (rootElement) => {
  // root.unmount()
};
```

Uses React 18+ `createRoot` API. Supports React 19.

### Hooks

```javascript
// In your .ferridriver-ct/index.ts (or equivalent setup file):
window.__ferriBeforeMount = window.__ferriBeforeMount || [];
window.__ferriAfterMount = window.__ferriAfterMount || [];

// Example: inject a theme provider before every mount
window.__ferriBeforeMount.push(async ({ Component, props, hooksConfig }) => {
  // Modify props, set up providers, etc.
});

window.__ferriAfterMount.push(async ({ Component, props, rootElement, hooksConfig }) => {
  // Post-mount setup
});
```

## Configuration

The base URL for the CT server is resolved from:

1. `FERRIDRIVER_CT_URL` env var (set automatically by `createCtRunner`)
2. `CT_URL` env var (for manual dev server)
3. Default: `http://localhost:3100`

## Exports

```javascript
import { registerSourcePath, frameworkName, vitePlugin } from '@ferridriver/ct-react';

registerSourcePath  // Absolute path to registerSource.mjs (for ct-core's Vite plugin)
frameworkName       // "react"
vitePlugin()        // Returns @vitejs/plugin-react (async)
```

## Integration with ct-core

```javascript
import { createCtRunner } from '@ferridriver/ct-core';
import { registerSourcePath, vitePlugin } from '@ferridriver/ct-react';

const runner = await createCtRunner({
  projectDir: process.cwd(),
  testFiles: ['./src/Counter.ct.test.tsx'],
  registerSourcePath,
  frameworkPlugin: vitePlugin,
});
// runner.baseUrl => http://127.0.0.1:3100/__ferri_ct_index.html
```
