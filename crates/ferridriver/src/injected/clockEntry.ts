/**
 * Standalone entry for the fake-clock engine (Playwright
 * injected/src/clock.ts, vendored verbatim as ./clock.ts).
 *
 * Compiled to dist/clock.min.js and delivered via addInitScript by the
 * Rust `Clock` surface (`crates/ferridriver/src/clock.rs`). The bundle
 * only defines the installer; the Rust side appends a
 * `__ferriClockInstall("<browserName>")` call so the per-context
 * browser name reaches AbortSignal.timeout's error-message quirk.
 */

import { inject } from './clock';

(globalThis as any).__ferriClockInstall = (browserName?: string) => {
  const g = globalThis as any;
  if (!g.__pwClock)
    g.__pwClock = inject(globalThis as any, browserName);
  return g.__pwClock;
};
