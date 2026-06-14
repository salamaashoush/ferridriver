/**
 * ferridriver injected script entry point.
 *
 * Uses Playwright's full InjectedScript class for selectors, actionability,
 * and element actions. Adds ferridriver-specific helpers (markdown, search, etc.)
 *
 * Core engine from Microsoft Playwright (Apache 2.0).
 */

import { InjectedScript } from './injectedScript';
import { isElementVisible, parentElementOrShadowHost, enclosingShadowRootOrDocument } from './domUtils';
import { getAriaDisabled, getAriaRole, getCheckedWithoutMixed, getElementAccessibleName, getReadonly } from './roleUtils';
import { escapeForTextSelector, escapeForAttributeSelector } from '@isomorphic/stringUtils';
import { UtilityScript } from './utilityScript';
import { parseEvaluationResultValue, serializeAsCallArgument } from '@isomorphic/utilityScriptSerializers';

// ── Types ──

type SelectorPart = { engine: string; body: string };

// ── Create InjectedScript instance ──

const injected = new InjectedScript(window, {
  isUnderTest: false,
  sdkLanguage: 'javascript',
  testIdAttributeName: 'data-testid',
  // Matches Playwright's `rafCountForStablePosition()` — 1 for Chromium,
  // Firefox, and WebKit on Linux/macOS (only WebKit-Windows uses 5, which
  // ferridriver does not target). 2 made the `stable` gate wait an extra
  // animation frame per pointer action vs Playwright.
  stableRafCount: 1,
  browserName: 'chromium',
  customEngines: [],
});

// ── Selector Execution (compatibility with ferridriver's parts-based API) ──

// Convert ferridriver's {engine, body} parts to a Playwright selector string.
// body format from Rust: raw text for text/label, attribute value for placeholder/alt/title,
// role spec for role, CSS for css, etc.
// exact flag: body enclosed in double quotes = exact match, otherwise substring/case-insensitive.
function partsToSelectorString(parts: SelectorPart[]): string {
  return parts.map(p => {
    const engine = p.engine;
    const body = p.body;
    // Detect exact match: Playwright convention is body wrapped in double quotes
    const isExact = body.startsWith('"') && body.endsWith('"');
    const rawBody = isExact ? body.slice(1, -1) : body;

    switch (engine) {
      case 'text':
        return `internal:text=${escapeForTextSelector(rawBody, isExact)}`;
      case 'label':
        return `internal:label=${escapeForTextSelector(rawBody, isExact)}`;
      case 'placeholder':
        return `internal:attr=[placeholder=${escapeForAttributeSelector(rawBody, isExact)}]`;
      case 'alt':
        return `internal:attr=[alt=${escapeForAttributeSelector(rawBody, isExact)}]`;
      case 'title':
        return `internal:attr=[title=${escapeForAttributeSelector(rawBody, isExact)}]`;
      case 'testid':
        // testid is always exact match
        return `internal:testid=[data-testid=${escapeForAttributeSelector(rawBody, true)}]`;
      case 'role':
        // role body is already in Playwright format: button[name="Save"][checked=true]...
        return `internal:role=${body}`;
      case 'has':
        return `internal:has="${body.replace(/"/g, '\\"')}"`;
      case 'has-not':
        return `internal:has-not="${body.replace(/"/g, '\\"')}"`;
      case 'has-text':
        return `internal:has-text=${escapeForTextSelector(rawBody, isExact)}`;
      case 'has-not-text':
        return `internal:has-not-text=${escapeForTextSelector(rawBody, isExact)}`;
      // Playwright-native internal:* engines emitted by ferridriver's
      // regex-capable getBy* builders. Body is already in Playwright's
      // internal format (`"quoted"i`/`"quoted"s`/`/regex/flags` for
      // text/label; `[name=<escaped>]` for attr/testid; role spec for
      // role) — pass through verbatim so the native Playwright selector
      // engine does the matching (including RegExp).
      case 'internal:text':
      case 'internal:label':
      case 'internal:attr':
      case 'internal:testid':
      case 'internal:role':
      case 'internal:has':
      case 'internal:has-not':
      case 'internal:has-text':
      case 'internal:has-not-text':
      case 'internal:and':
      case 'internal:or':
        return `${engine}=${body}`;
      default:
        // css, xpath, id, nth, visible - pass through
        return `${engine}=${body}`;
    }
  }).join(' >> ');
}

function executeSelector(parts: SelectorPart[], root: Node): Element[] {
  const selectorStr = partsToSelectorString(parts);
  try {
    const parsed = injected.parseSelector(selectorStr);
    return injected.querySelectorAll(parsed, root);
  } catch {
    // Fallback: try each part as CSS
    let current: Element[] = root === document ? [document.documentElement] : [root as Element];
    for (const part of parts) {
      if (part.engine === 'nth') {
        const idx = parseInt(part.body, 10);
        current = (idx >= 0 && idx < current.length) ? [current[idx]] : [];
        continue;
      }
      const next: Element[] = [];
      const seen = new Set<Element>();
      for (const el of current) {
        let found: Element[] = [];
        try {
          if (part.engine === 'css') found = [...el.querySelectorAll(part.body)];
          else if (part.engine === 'xpath') {
            const doc = el.ownerDocument || document;
            const it = doc.evaluate(part.body.startsWith('/') ? '.' + part.body : part.body, el, null, XPathResult.ORDERED_NODE_ITERATOR_TYPE);
            for (let n = it.iterateNext(); n; n = it.iterateNext()) {
              if (n.nodeType === Node.ELEMENT_NODE) found.push(n as Element);
            }
          } else if (part.engine === 'id') {
            const e = document.getElementById(part.body);
            if (e) found = [e];
          }
        } catch {}
        for (const f of found) {
          if (!seen.has(f)) { seen.add(f); next.push(f); }
        }
      }
      current = next;
    }
    return current;
  }
}

// ── Actionability (delegates to Playwright's InjectedScript) ──

type ElementState = 'visible' | 'hidden' | 'enabled' | 'disabled' | 'editable' | 'stable' | 'checked' | 'unchecked' | 'indeterminate';

function elementState(el: Element, state: ElementState): boolean | 'error:notconnected' {
  const result = injected.elementState(el, state);
  if (typeof result === 'object' && result !== null && 'matches' in result) {
    return (result as any).matches;
  }
  return result as any;
}

function checkElementStates(el: Element, states: ElementState[]): string {
  for (const state of states) {
    const result = elementState(el, state);
    if (result === 'error:notconnected') return 'error:notconnected';
    if (!result) return `error:not${state}`;
  }
  return 'done';
}

function isActionable(el: Element): { actionable: boolean; reason?: string } {
  if (!el.isConnected) return { actionable: false, reason: 'notconnected' };
  if (!isElementVisible(el)) return { actionable: false, reason: 'notvisible' };
  if (getAriaDisabled(el)) return { actionable: false, reason: 'disabled' };
  return { actionable: true };
}

// ── Hit Target Testing (delegates to Playwright) ──

function expectHitTarget(hitPoint: { x: number; y: number }, targetElement: Element): 'done' | { hitTargetDescription: string } {
  return injected.expectHitTarget(hitPoint, targetElement);
}

/**
 * Wrap Playwright's `setupHitTargetInterceptor` (defined in the bundled
 * `injectedScript.ts`) so the Rust click path can install + finalize
 * without managing a JS handle round-trip. State lives on
 * `window.__fd._hitInterceptor` for the duration of one click.
 *
 * Mirrors `_performPointerAction` in
 * `/tmp/playwright/packages/playwright-core/src/server/dom.ts:393`:
 * Playwright sets up the interceptor before the CDP mouse events,
 * dispatches the events, then calls `handle.stop()` to read the
 * captured hit target. We do the same — `installHitInterceptor`
 * before mouseMoved, `finalizeHitInterceptor` after mouseReleased.
 *
 * Returns `'ok'` on successful install (or the preliminary
 * `hitTargetDescription` string if the requested point is already
 * occluded), and `'done'` / `{ hitTargetDescription }` on finalize.
 */
function installHitInterceptor(
  el: Element,
  hitPoint: { x: number; y: number },
  action: 'hover' | 'tap' | 'mouse' | 'drag' = 'mouse',
): 'ok' | string {
  const w: any = window;
  // Tear down any previous interceptor; a stuck listener from a prior
  // miss-retry would otherwise swallow the next click's events.
  if (w.__fd && w.__fd._hitInterceptor) {
    try {
      w.__fd._hitInterceptor.stop();
    } catch {}
    w.__fd._hitInterceptor = null;
  }
  const r = injected.setupHitTargetInterceptor(el, action, hitPoint, false);
  if (typeof r === 'string') return r;
  if (r === 'error:notconnected') return r;
  w.__fd._hitInterceptor = r;
  return 'ok';
}

function finalizeHitInterceptor(): 'done' | { hitTargetDescription: string } {
  const w: any = window;
  const it = w.__fd && w.__fd._hitInterceptor;
  if (!it) return 'done';
  w.__fd._hitInterceptor = null;
  return it.stop();
}

// ── Click Guard ──

function clickGuard(el: Element): string {
  const tag = el.tagName?.toUpperCase();
  if (tag === 'SELECT') return 'select';
  if (tag === 'INPUT' && (el as HTMLInputElement).type === 'file') return 'file';
  return '';
}

/**
 * Combined click pre-flight check. Replaces FOUR sequential
 * `Runtime.callFunctionOn` round-trips (clickGuard + isActionable +
 * scrollIntoView + resolveClickPoint) with a single call:
 *
 * 1. `clickGuard` — reject `<select>` / file inputs page-side so the
 *    Rust action helper can dispatch a typed error.
 * 2. `isActionable` — returns `actionable: true` only when the
 *    element is connected, visible, and not aria-disabled.
 * 3. `scrollIntoViewIfNeeded` — non-standard Chromium primitive;
 *    falls back to W3C `scrollIntoView({block:'center'})` on
 *    Firefox/BiDi.
 * 4. Iframe-chain accumulated bounding-box → click point.
 *
 * Returns a flat object so the host can branch on `guard`/`reason`
 * without further parsing. `point` is `null` when the element is
 * not actionable (caller short-circuits on guard / reason before
 * touching point). Mirrors Playwright's `evaluateInUtility` pattern
 * in `dom.ts::_performPointerAction` which similarly batches these
 * checks into one CDP RTT.
 */
/**
 * Run Playwright's full actionability gate via the vendored
 * `injected.checkElementStates` (`'stable'` = async rAF bounding-box
 * sampling). Returns `undefined` when every requested state passes,
 * otherwise the failing state name so the Rust host formats
 * `error:not<state>` and retries.
 *
 * The `'stable'` check is rAF-driven, and `requestAnimationFrame` is
 * throttled (or paused) on a backgrounded page — e.g. any non-foreground
 * tab in a multi-page context. To stop that from stalling an action
 * indefinitely, the stable wait races a `setTimeout` watchdog (timers keep
 * firing thanks to `--disable-background-timer-throttling`): if rAF hasn't
 * settled within the window, treat the element as stable — a page whose rAF
 * is throttled is not animating fast enough to matter.
 */
async function awaitStates(el: Element, states: ElementState[]): Promise<string | undefined> {
  // Non-stable states (visible / enabled / editable) are synchronous — no
  // rAF — so check them directly.
  const nonStable = states.filter((s) => s !== 'stable');
  const result = await injected.checkElementStates(el, nonStable as any);
  if (result === 'error:notconnected') return 'connected';
  if (result !== undefined) return (result as any).missingState;

  if (states.includes('stable')) {
    const stable = await Promise.race([
      injected.checkElementStates(el, ['stable'] as any).then((r: any) => r === undefined),
      new Promise<boolean>((resolve) => setTimeout(() => resolve(true), 1000)),
    ]);
    if (!stable) return 'stable';
  }
  return undefined;
}

async function clickPrep(
  el: Element,
  position: { x: number; y: number } | null,
  states: ElementState[],
  scrollAlign?: ScrollIntoViewOptions | null,
): Promise<{
  guard: string;
  actionable: boolean;
  reason?: string;
  point: { x: number; y: number } | null;
}> {
  const guard = clickGuard(el);
  if (guard) return { guard, actionable: false, point: null };
  // Full actionability (visible / enabled / stable as requested) before
  // resolving the click point — mirrors Playwright's `_performPointerAction`
  // order (states gate first, then scroll + clickable point).
  const reason = await awaitStates(el, states);
  if (reason) return { guard: '', actionable: false, reason, point: null };
  // Scroll into view. On a retry the host passes an explicit alignment
  // (Playwright `_retryPointerAction` cycles undefined/end/center/start to
  // escape position:sticky overlays); otherwise use the precise non-standard
  // `scrollIntoViewIfNeeded`.
  if (scrollAlign) {
    el.scrollIntoView(scrollAlign);
  } else if (typeof (el as any).scrollIntoViewIfNeeded === 'function') {
    (el as any).scrollIntoViewIfNeeded();
  } else {
    el.scrollIntoView({ block: 'center', inline: 'center' });
  }
  let x: number;
  let y: number;
  if (position) {
    // Playwright `_offsetPoint`: `position` is relative to the PADDING box,
    // so add the top/left border widths (a border-box-relative offset would
    // land `borderWidth` px off).
    const r = el.getBoundingClientRect();
    const cs = (el.ownerDocument as Document).defaultView!.getComputedStyle(el);
    x = r.x + (parseFloat(cs.borderLeftWidth) || 0) + position.x;
    y = r.y + (parseFloat(cs.borderTopWidth) || 0) + position.y;
  } else {
    // Playwright `_clickablePoint`: pick the largest viewport-intersected
    // client rect's centre. Approximated page-side via `getClientRects()`
    // (one rect per line box) instead of CDP `DOM.getContentQuads`, so it
    // stays in this single round-trip. Avoids the inter-line gap a wrapped
    // inline element's bounding-box centre falls into, and keeps the point
    // inside the viewport for partially-scrolled elements.
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    let best: DOMRect | null = null;
    let bestArea = -1;
    for (const rc of Array.from(el.getClientRects())) {
      const iw = Math.max(0, Math.min(rc.right, vw) - Math.max(rc.left, 0));
      const ih = Math.max(0, Math.min(rc.bottom, vh) - Math.max(rc.top, 0));
      const area = iw * ih;
      if (area > bestArea) {
        bestArea = area;
        best = rc;
      }
    }
    if (!best || bestArea <= 0.99) {
      // No client rect intersects the viewport (Playwright `_clickablePoint`
      // returns `error:notinviewport`). The host retries with the next scroll
      // alignment to bring it on-screen.
      return { guard: '', actionable: false, reason: 'inviewport', point: null };
    }
    x = (Math.max(best.left, 0) + Math.min(best.right, vw)) / 2;
    y = (Math.max(best.top, 0) + Math.min(best.bottom, vh)) / 2;
  }
  let win: any = (el.ownerDocument as Document).defaultView;
  while (win && win !== win.parent && win.frameElement) {
    const fr = (win.frameElement as Element).getBoundingClientRect();
    x += fr.x;
    y += fr.y;
    win = win.parent;
  }
  return { guard: '', actionable: true, point: { x, y } };
}

// ── Actions (delegates to Playwright where possible) ──

function clearAndDispatch(el: HTMLInputElement | HTMLTextAreaElement, value?: string) {
  el.focus();
  el.value = '';
  if (value !== undefined) el.value = value;
  el.dispatchEvent(new Event('input', { bubbles: true }));
  el.dispatchEvent(new Event('change', { bubbles: true }));
}

function dispatchInputEvents(el: Element) {
  el.dispatchEvent(new Event('input', { bubbles: true }));
  el.dispatchEvent(new Event('change', { bubbles: true }));
}

function fillElement(el: Element, value: string): 'done' | 'needsinput' | 'error:notconnected' {
  return injected.fill(el, value);
}

function selectOptions(el: Element, ...options: { value?: string; label?: string; index?: number; valueOrLabel?: string }[]): string[] | string {
  return injected.selectOptions(el, options);
}

function getOptions(el: HTMLSelectElement): { options: { index: number; text: string; value: string; selected: boolean }[] } {
  return {
    options: [...el.options].map((o, i) => ({
      index: i,
      text: (o.textContent || '').trim(),
      value: o.value,
      selected: o.selected,
    }))
  };
}

// Legacy selectOption (single string target) for backward compatibility
function selectOption(el: HTMLSelectElement, target: string): { selected: boolean; value?: string; error?: string } {
  const options = [...el.options];
  const opt = options.find(o => o.value === target || normalizeWS(o.textContent || '') === normalizeWS(target));
  if (!opt) return { selected: false, error: 'Option not found' };
  el.value = opt.value;
  el.dispatchEvent(new Event('input', { bubbles: true }));
  el.dispatchEvent(new Event('change', { bubbles: true }));
  return { selected: true, value: opt.value };
}

function focusNode(el: Element) {
  injected.focusNode(el, false);
}

function blurNode(el: Element) {
  injected.blurNode(el);
}

function selectText(el: Element) {
  injected.selectText(el);
}

function setInputFiles(el: Element, payloads: { name: string; mimeType: string; buffer: string }[]) {
  return injected.setInputFiles(el, payloads);
}

function normalizeWS(s: string): string {
  return (s || '').replace(/[\u200b\u00ad]/g, '').trim().replace(/\s+/g, ' ');
}

// ── Install on window.__fd ──

declare global {
  interface Window { __fd: any; }
}

if (!window.__fd) {
  window.__fd = {
    // Playwright InjectedScript instance (for advanced/direct use)
    _injected: injected,

    // Selector API (backward compatible)
    _exec: executeSelector,
    sel(parts: SelectorPart[]) {
      try {
        const results = executeSelector(parts, document);
        results.forEach((el, i) => el.setAttribute('data-fd-sel', '' + i));
        return JSON.stringify(results.map((el, i) => {
          const text = (el.textContent || '').trim();
          return { index: i, tag: el.tagName.toLowerCase(), text: text.length > 100 ? text.slice(0, 100) + '...' : text };
        }));
      } catch (e: any) { return JSON.stringify({ error: e.message }); }
    },
    /**
     * Resolve `parts` to a single element. When `strict` is true and the
     * selector matches more than one element, throw a recognisable
     * `strict mode violation: <count>` error so the host (Rust) can
     * convert it to a typed `FerriError::StrictModeViolation` without a
     * separate `query_all` round-trip. Mirrors Playwright's
     * `injected.querySelector(selector, root, strict)` pattern in
     * `/tmp/playwright/packages/injected/src/injectedScript.ts:276`.
     */
    selOne(parts: SelectorPart[], strict?: boolean) {
      const r = executeSelector(parts, document);
      if (strict && r.length > 1) {
        // Encode the count in a parseable token. Rust regexes
        // `strict mode violation: <count>` to extract the hit count
        // and build a typed StrictModeViolation error.
        throw new Error('strict mode violation: ' + r.length);
      }
      return r.length > 0 ? r[0] : null;
    },
    selAll(parts: SelectorPart[]) { return executeSelector(parts, document); },
    selCount(parts: SelectorPart[]) { return executeSelector(parts, document).length; },
    scrollInfo() {
      return JSON.stringify({ scrollY: window.scrollY, scrollHeight: document.documentElement.scrollHeight, viewportHeight: window.innerHeight });
    },

    /**
     * Resolve `parts` to a single element (strict) and return the
     * canonical selector Playwright's recorder/codegen would emit for
     * it. Mirrors `Frame.resolveSelector` in
     * `/tmp/playwright/packages/playwright-core/src/server/frames.ts:1274`
     * which calls `injected.generateSelectorSimple(node)`. Throws
     * `strict mode violation: <count>` on >1 match and a plain error on
     * 0 matches so the Rust host can surface a typed error.
     */
    normalizeSelector(parts: SelectorPart[]): string {
      const r = executeSelector(parts, document);
      if (r.length === 0)
        throw new Error('normalize: no element found');
      if (r.length > 1)
        throw new Error('strict mode violation: ' + r.length);
      return injected.generateSelectorSimple(r[0]);
    },

    // Playwright selector API (direct access)
    parseSelector: (s: string) => injected.parseSelector(s),
    querySelector: (selector: any, root: Node, strict: boolean) => injected.querySelector(selector, root, strict),
    querySelectorAll: (selector: any, root: Node) => injected.querySelectorAll(selector, root),

    // Element highlight overlay (Playwright `injected.addHighlight` /
    // `removeHighlight` / `hideHighlight`). Mirrors
    // packages/injected/src/injectedScript.ts addHighlight/removeHighlight.
    // `parts` is ferridriver's {engine, body} array — converted to a
    // Playwright ParsedSelector so the highlight RAF loop can re-resolve
    // it as the DOM/layout changes. `cssStyle` is the resolved CSS string
    // (Locator.highlight composes the optional style record host-side).
    addHighlight(parts: SelectorPart[], cssStyle?: string) {
      injected.addHighlight(injected.parseSelector(partsToSelectorString(parts)), cssStyle);
    },
    removeHighlight(parts: SelectorPart[]) {
      injected.removeHighlight(injected.parseSelector(partsToSelectorString(parts)));
    },
    hideHighlight() {
      injected.hideHighlight();
    },

    // Actionability
    elementState,
    checkElementStates,
    isActionable,
    isVisible: isElementVisible,
    expectHitTarget,
    installHitInterceptor,
    finalizeHitInterceptor,

    // Actions (Playwright-ported)
    clearAndDispatch,
    dispatchInputEvents,
    clickGuard,
    clickPrep,
    awaitStates,
    selectOption,
    selectOptions,
    getOptions,
    fill: fillElement,
    focusNode,
    blurNode,
    selectText,
    setInputFiles,

    // ARIA
    getAriaRole,
    getAccessibleName: getElementAccessibleName,
    getAriaDisabled,
    getChecked: getCheckedWithoutMixed,
    getReadonly,

    // Playwright's aria snapshot (locator/page scoped). `node` is the
    // root resolved by Rust (strict resolution + auto-wait done
    // host-side — source of truth); delegates to the vendored
    // InjectedScript so the rendered YAML is byte-for-byte Playwright.
    ariaSnapshot: (node: Node, options?: { mode?: 'ai' | 'default'; depth?: number }) =>
      injected.ariaSnapshot(node, { mode: options?.mode || 'default', depth: options?.depth }),
    // Full result incl. `iframeRefs` / `iframeDepths` so the Rust core
    // can stitch child-iframe subtrees (mirrors server
    // `ariaSnapshotForFrame`). `refPrefix` namespaces refs per frame so
    // the parent's `- iframe [ref=...]` line is unique and resolvable.
    incrementalAriaSnapshot: (
      node: Node,
      options?: { mode?: 'ai' | 'default'; depth?: number; refPrefix?: string; boxes?: boolean },
    ) =>
      injected.incrementalAriaSnapshot(node, {
        mode: options?.mode || 'default',
        depth: options?.depth,
        refPrefix: options?.refPrefix,
        boxes: options?.boxes,
      }),
    // Tag the iframe/frame element that the renderer assigned `ref` to
    // with `attr=ref`, so the host can re-resolve it through the normal
    // selector + content-frame path (BiDi-safe — passing a utility-eval
    // handle into the cross-context content-frame call is not). Returns
    // whether a matching element was found.
    markIframeByAriaRef: (ref: string, attr: string): boolean => {
      const all = document.querySelectorAll('iframe,frame');
      for (const el of all) {
        const r = (el as any)._ariaRef;
        if (r && r.ref === ref) {
          el.setAttribute(attr, ref);
          return true;
        }
      }
      return false;
    },

    // ── Evaluate(fn, arg) plumbing ──
    //
    // Playwright's utility-script machinery, lifted verbatim from
    // packages/injected/src/utilityScript.ts. The Rust side creates one
    // `UtilityScript` instance per execution context (materialized as a
    // JSHandle via CDP `Runtime.evaluate` on
    // `window.__fd.newUtilityScript()`) and then every subsequent
    // `page.evaluate(fn, arg)` / `locator.evaluate(fn, arg)` call goes
    // through CDP `Runtime.callFunctionOn` with the utility-script
    // handle as the receiver. The function body is
    //   `(utilityScript, ...args) => utilityScript.evaluate(...args)`
    // and `utilityScript.evaluate(isFunction, returnByValue, expression,
    // argCount, ...argsAndHandles)` reconstructs each serialized arg via
    // `parseEvaluationResultValue`, invokes the user function, and
    // serializes the result back with `serializeAsCallArgument`.
    UtilityScript,
    newUtilityScript: () => new UtilityScript(window as any, false),
    parseEvaluationResultValue,
    serializeAsCallArgument,
  };
}
