// Test helpers: backend-matrix launcher.
//
// `Browser.launch({ backend })` no longer exists — the new
// Playwright-shaped API is `chromium()` / `firefox()` / `webkit()`.
// `cdp-raw` access is via `chromium({ transport: 'ws' })`. This
// helper preserves the per-backend test matrix that every NAPI test
// file relies on.

import { chromium, firefox, webkit, type Browser } from "../index.js";

/**
 * Launch a browser for a backend identifier. Used by the per-backend
 * matrix in NAPI tests (`for (const backend of BACKENDS) ...`).
 *
 * - `cdp-pipe` -> `chromium().launch()`
 * - `cdp-raw`  -> `chromium({ transport: 'ws' }).launch()`
 * - `bidi`     -> `firefox().launch()`
 * - `webkit`   -> `webkit().launch()` (macOS only)
 */
export function launchForBackend(backend: string): Promise<Browser> {
  switch (backend) {
    case "cdp-pipe":
      return chromium().launch();
    case "cdp-raw":
      return chromium({ transport: "ws" }).launch();
    case "bidi":
      return firefox().launch();
    case "webkit":
      return webkit().launch();
    default:
      throw new Error(`Unknown backend: ${backend}`);
  }
}
