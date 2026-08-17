/**
 * The multi-attribute `internal:testid` engine, as a custom engine.
 *
 * `use: { testIdAttribute: 'data-pw,data-ti' }` names more than one
 * attribute, and a `getByTestId` selector then carries the whole list:
 * `internal:testid=["data-pw,data-ti"=value]`. Upstream grew that in
 * `_createTestIdEngine`; ferridriver's vendored `injectedScript.ts` is
 * still on the single-attribute copy (see VENDOR.md — taking the newer
 * one waits on the aria rendering port), so the same rule lives here and
 * is registered under the built-in's name.
 *
 * `InjectedScript` installs `options.customEngines` AFTER its built-ins,
 * so registering `internal:testid` replaces the stale one rather than
 * fighting it — a documented extension point, not a patch. Delete this
 * file, and the registration in `index.ts`, once the vendored
 * `injectedScript.ts` catches up.
 *
 * Ported from `packages/injected/src/injectedScript.ts`
 * (`_createTestIdEngine` + `createAttributeMatcher`) at 1.63.0-next.
 */

import { parseAttributeSelector } from '@isomorphic/selectorParser';
import { splitTestIdAttributeNames } from '@isomorphic/locatorUtils';

import type { AttributeSelectorPart } from '@isomorphic/selectorParser';

type SelectorRoot = Document | ShadowRoot | Element;

function createAttributeMatcher(part: AttributeSelectorPart): (s: string) => boolean {
  const { value, caseSensitive } = part;
  if (value instanceof RegExp)
    return s => !!s.match(value);
  if (caseSensitive)
    return s => s === value;
  const lowerCaseValue = String(value).toLowerCase();
  return s => s.toLowerCase().includes(lowerCaseValue);
}

/**
 * `queryAll` over every named attribute. The engine is handed the
 * injected script so it can reuse the shadow-piercing CSS query the
 * built-ins use — a plain `querySelectorAll` would stop at the first
 * shadow root.
 */
export function createTestIdEngine(injected: { _evaluator: { _queryCSS: (ctx: { scope: Document | Element, pierceShadow: boolean }, css: string) => Element[] } }) {
  const queryAll = (root: SelectorRoot, selector: string): Element[] => {
    const parsed = parseAttributeSelector(selector, true);
    if (parsed.name || parsed.attributes.length !== 1)
      throw new Error('Malformed test id selector: ' + selector);
    const names = splitTestIdAttributeNames(parsed.attributes[0].name);
    const matcher = createAttributeMatcher(parsed.attributes[0]);
    const cssQuery = names.map(n => `[${n}]`).join(',');
    const elements = injected._evaluator._queryCSS({ scope: root as Document | Element, pierceShadow: true }, cssQuery);
    return elements.filter(e => names.some(n => {
      const actual = e.getAttribute(n);
      return actual !== null && matcher(actual);
    }));
  };
  return { queryAll };
}
