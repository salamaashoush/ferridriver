/**
 * On-demand bundle: ARIA snapshot generation, YAML rendering, and
 * template parsing/matching (`window.__fdAria`).
 *
 * Evaluated ONLY when an ARIA snapshot or `toMatchAriaSnapshot` API
 * runs, or once on a timed-out `expect()` failure to attach error
 * context. Keeping the YAML parser, ARIA tree distiller/renderer, and
 * template matcher out of the always-injected engine removes ~20 KB
 * (plus the `yaml` package) from every document injection.
 */

import { parseAriaSnapshot, parseAriaSnapshotUnsafe } from '@isomorphic/ariaSnapshot';
import { renderAriaSnapshotAsYaml } from '@isomorphic/ariaSnapshotRenderer';
import {
  findNewElement,
  generateAriaTree,
  getAllElementsMatchingExpectAriaTemplate,
  matchesExpectAriaTemplate,
  renderAriaTreeAsJSON,
} from './ariaSnapshot';
import * as yaml from 'yaml';

declare global {
  interface Window {
    __fd: any;
    __fdAria: {
      parse: (text: string) => unknown;
      parseAriaSnapshot: typeof parseAriaSnapshot;
      generateAriaTree: typeof generateAriaTree;
      renderAriaTreeAsJSON: typeof renderAriaTreeAsJSON;
      renderAriaSnapshotAsYaml: typeof renderAriaSnapshotAsYaml;
      matchesExpectAriaTemplate: typeof matchesExpectAriaTemplate;
      getAllElementsMatchingExpectAriaTemplate: typeof getAllElementsMatchingExpectAriaTemplate;
      findNewElement: typeof findNewElement;
    };
  }
}

(() => {
  if (window.__fdAria instanceof Object)
    return;
  window.__fdAria = {
    parse: (text: string) => parseAriaSnapshotUnsafe(yaml as any, text),
    parseAriaSnapshot,
    generateAriaTree,
    renderAriaTreeAsJSON,
    renderAriaSnapshotAsYaml,
    matchesExpectAriaTemplate,
    getAllElementsMatchingExpectAriaTemplate,
    findNewElement,
  };
})();
