/**
 * On-demand bundle: the aria-snapshot TEMPLATE PARSER.
 *
 * `expect(locator).toMatchAriaSnapshot(yaml)` compares a YAML template
 * against the live aria tree. Playwright splits that in two — the
 * server parses the YAML into an `AriaTemplateNode`
 * (`isomorphic/ariaSnapshot.ts::parseAriaSnapshotUnsafe`) and the
 * injected script matches the parsed template against the tree — so the
 * `yaml` library never ships to the page.
 *
 * ferridriver has no in-page Node, so the parse happens here instead,
 * in a bundle that is evaluated ONLY when an aria-snapshot assertion
 * runs. The always-injected engine keeps just the matcher
 * (`__fd.matchAriaTemplate`).
 */

import { parseAriaSnapshotUnsafe } from '@isomorphic/ariaSnapshot';
import * as yaml from 'yaml';

declare global {
  interface Window {
    __fd: any;
    __fdAria: { parse: (text: string) => unknown };
  }
}

window.__fdAria = {
  parse: (text: string) => parseAriaSnapshotUnsafe(yaml as any, text),
};
