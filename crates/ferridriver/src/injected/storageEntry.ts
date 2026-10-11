/**
 * Standalone entry for the storage-state script (Playwright
 * injected/src/storageScript.ts, vendored verbatim as ./storageScript.ts).
 *
 * Compiled to dist/storage.min.js, an expression that evaluates to the
 * script's class. `context.storageState()` and `setStorageState()` put it
 * in front of a `collect` or `restore` call in the page of each origin,
 * so nothing is left on the page's globals.
 */

import { StorageScript } from './storageScript';

// Declared by the wrapper `build.ts` puts around this bundle.
declare let __fdStorageScript: typeof StorageScript;

__fdStorageScript = StorageScript;
