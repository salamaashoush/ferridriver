/**
 * Node-by-node aria tree equality — ferridriver's, not Playwright's.
 *
 * Upstream carried `ariaNodesEqual` / `ariaPropsEqual` in
 * `isomorphic/ariaSnapshot.ts` until it split snapshot rendering into
 * `renderAriaTreeAsJSON` + `isomorphic/ariaSnapshotRenderer`, which
 * removed them. ferridriver's incremental snapshot still needs them:
 * `__fd.incrementalAriaSnapshot` renders one tree and reports only what
 * changed against the previous one (`locator.ariaSnapshot({ track })`,
 * and the MCP snapshot tool, which would otherwise re-send the whole
 * page on every step).
 *
 * Kept here rather than patched back into the vendored file so every
 * file under `injected/` and `injected/isomorphic/` stays byte-identical
 * to upstream and the next sync is a copy, not a merge. See
 * `injected/VENDOR.md`.
 */

import { hasPointerCursor } from '@isomorphic/ariaSnapshot';

import type { AriaNode, AriaProps } from '@isomorphic/ariaSnapshot';

export function ariaNodesEqual(a: AriaNode, b: AriaNode): boolean {
  if (a.role !== b.role || a.name !== b.name)
    return false;
  if (!ariaPropsEqual(a, b) || hasPointerCursor(a) !== hasPointerCursor(b))
    return false;
  const aKeys = Object.keys(a.props);
  const bKeys = Object.keys(b.props);
  return aKeys.length === bKeys.length && aKeys.every(k => a.props[k] === b.props[k]);
}

function ariaPropsEqual(a: AriaProps, b: AriaProps): boolean {
  return a.active === b.active
    && a.checked === b.checked
    && a.disabled === b.disabled
    && a.expanded === b.expanded
    && a.selected === b.selected
    && a.level === b.level
    && a.pressed === b.pressed;
}
