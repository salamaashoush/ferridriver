/**
 * Playwright-compatible error classes.
 *
 * The Rust core raises `FerriError`; the NAPI bridge tags timeout and
 * target-closed errors with a `"<Name>: "` message prefix so consumers can
 * dispatch on error class. This module exports the class hierarchy and a
 * single `promoteError` helper that rethrows NAPI errors as the matching
 * class — giving callers `err instanceof TimeoutError` and
 * `err.name === 'TimeoutError'`, exactly like Playwright.
 *
 * Usage:
 *   try { await page.click('button'); }
 *   catch (e) {
 *     if (e instanceof TimeoutError) { ... }
 *   }
 *
 * See `crates/ferridriver-node/src/error.rs` for the Rust side.
 */

export class TimeoutError extends Error {
  override readonly name = 'TimeoutError';
  constructor(message: string) {
    super(message);
    Object.setPrototypeOf(this, TimeoutError.prototype);
  }
}

export class TargetClosedError extends Error {
  override readonly name = 'TargetClosedError';
  constructor(message: string) {
    super(message);
    Object.setPrototypeOf(this, TargetClosedError.prototype);
  }
}

/**
 * Matches Playwright's `errors.TimeoutError.isTimeoutError(err)` style helper.
 */
export function isTimeoutError(err: unknown): err is TimeoutError {
  return err instanceof TimeoutError || (err instanceof Error && err.name === 'TimeoutError');
}

export function isTargetClosedError(err: unknown): err is TargetClosedError {
  return err instanceof TargetClosedError || (err instanceof Error && err.name === 'TargetClosedError');
}

/**
 * Rethrow a NAPI error as the matching class if its message carries a known
 * prefix; otherwise return it unchanged. Wrappers around NAPI calls funnel
 * caught errors through this helper so consumers see proper Playwright
 * error types.
 *
 * Message shape produced by `crates/ferridriver-node/src/error.rs`:
 *   "TimeoutError: Timeout 30000ms exceeded while navigating"
 *   "TargetClosedError: Target page, context or browser has been closed"
 */
export function promoteError(err: unknown): Error {
  if (!(err instanceof Error)) return new Error(String(err));
  // Already promoted.
  if (err instanceof TimeoutError || err instanceof TargetClosedError) return err;

  const msg = err.message;
  if (msg.startsWith('TimeoutError: ')) {
    const promoted = new TimeoutError(msg.slice('TimeoutError: '.length));
    promoted.stack = err.stack;
    return promoted;
  }
  if (msg.startsWith('TargetClosedError: ')) {
    const promoted = new TargetClosedError(msg.slice('TargetClosedError: '.length));
    promoted.stack = err.stack;
    return promoted;
  }
  return err;
}

/**
 * Wrap an async callback so any thrown NAPI error is promoted to its typed
 * class before reaching the caller.
 */
export async function withPromotedErrors<T>(fn: () => Promise<T>): Promise<T> {
  try {
    return await fn();
  } catch (e) {
    throw promoteError(e);
  }
}

/**
 * Matches Playwright's `errors` namespace export.
 */
export const errors = {
  TimeoutError,
  TargetClosedError,
};
