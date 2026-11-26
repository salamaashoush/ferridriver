/**
 * ferridriver injected script entry point.
 *
 * Uses Playwright's full InjectedScript class for selectors, actionability,
 * and element actions. Adds ferridriver-specific helpers (markdown, search, etc.)
 *
 * Core engine from Microsoft Playwright (Apache 2.0).
 */

import { InjectedScript } from './injectedScript';
import { isElementVisible } from './domUtils';
import { getAriaDisabled, getAriaRole, getCheckedWithoutMixed, getElementAccessibleNameText, getReadonly } from './roleUtils';
import { escapeForTextSelector, escapeForAttributeSelector } from '@isomorphic/stringUtils';
import { UtilityScript } from './utilityScript';
import { parseEvaluationResultValue, serializeAsCallArgument } from '@isomorphic/utilityScriptSerializers';
import type { Language } from '@isomorphic/locatorGenerators';
import type { ParsedSelector } from '@isomorphic/selectorParser';

// ── Types ──

type SelectorPart = { engine: string; body: string; capture?: boolean };

// ── Create InjectedScript instance ──

/**
 * What the Rust side interpolates ahead of this bundle
 * (`selectors::build_lazy_inject_js_with`). Absent when the engine is
 * loaded by something that does not set it (a trace-viewer snapshot),
 * hence the defaults.
 */
type FdOptions = {
  testIdAttributeName?: string;
  customEngines?: { name: string, source: string }[];
};
const fdOptions: FdOptions = (window as any).__fdOptions ?? {};
// Capture timer/rAF builtins eagerly before page script runs (5us) so clock
// emulation and monkey-patched globals cannot affect later waits, while
// deferring the heavy `new InjectedScript(...)` construction (30 selector
// engines, Maps/Sets, 16 window capture listeners) until the first actual
// `window.__fd` call. Avoids constructing InjectedScript twice on every
// discarded `about:blank` and in unused utility worlds.
const initialBuiltins = new UtilityScript(window as any, false).builtins;
let _injected: InjectedScript | null = null;
function getInjected(): InjectedScript {
  if (!_injected) {
    _injected = new InjectedScript(window, {
      isUnderTest: false,
      sdkLanguage: 'javascript',
      // Playwright builds the injected options per FRAME and puts the
      // frame's sequence number in every `ai` ref (`f2e7`), so refs are
      // unique page-wide. ferridriver injects one script per PAGE (an
      // `addScriptToEvaluateOnNewDocument` source shared by every
      // document), so there is no per-frame number to put here: the host
      // renders each frame with its own ref namespace and prefixes the
      // child frame's refs while stitching (`locator.rs::aria_stitch_frame`).
      frameSeq: 0,
      // Cosmetic here: a `getByTestId` selector carries its attribute name
      // in the selector body, so this only feeds strict-mode error text and
      // codegen. Registered engines, however, exist only if listed here.
      testIdAttributeName: fdOptions.testIdAttributeName ?? 'data-testid',
      // Matches Playwright's `rafCountForStablePosition()` — 1 for Chromium,
      // Firefox, and WebKit on Linux/macOS (only WebKit-Windows uses 5, which
      // ferridriver does not target). 2 made the `stable` gate wait an extra
      // animation frame per pointer action vs Playwright.
      stableRafCount: 1,
      browserName: 'chromium',
      customEngines: fdOptions.customEngines ?? [],
    });
    _injected.utils.builtins = (window as any).__pwClock?.builtins ?? initialBuiltins;
  }
  return _injected;
}

// ── Selector Execution (compatibility with ferridriver's parts-based API) ──

// Convert ferridriver's {engine, body} parts to a Playwright selector string.
// body format from Rust: raw text for text/label, attribute value for placeholder/alt/title,
// role spec for role, CSS for css, etc.
// exact flag: body enclosed in double quotes = exact match, otherwise substring/case-insensitive.
function partsToSelectorString(parts: SelectorPart[]): string {
  if (parts.length === 1) {
    const p = parts[0];
    return (p.capture ? '*' : '') + partToSelectorString(p);
  }
  return parts.map(p => {
    // Playwright `*` capture modifier: re-emitted onto the converted
    // part so the ported parseSelector records the capture index.
    const star = p.capture ? '*' : '';
    const converted = partToSelectorString(p);
    return star + converted;
  }).join(' >> ');
}

function partToSelectorString(p: SelectorPart): string {
  {
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
        return `internal:has=${normalizeHasBody(body)}`;
      case 'has-not':
        return `internal:has-not=${normalizeHasBody(body)}`;
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
        // css, xpath, id, nth, visible, and Playwright-native engines
        // (`:light` variants, data-testid aliases, aria-ref,
        // internal:control, internal:chain) - pass through
        return `${engine}=${body}`;
    }
  }
}

// `internal:has=` arrives with a JSON-encoded inner selector (the
// Playwright client wire shape core's `filter()` emits); the bare
// `has=` shorthand in hand-written selector strings carries a raw
// selector. Normalize to exactly one level of JSON encoding — wrapping
// an already-encoded body a second time turns the inner selector into
// a quoted string that Playwright's parser reads as a text engine.
function normalizeHasBody(body: string): string {
  if (body.startsWith('"')) {
    try {
      JSON.parse(body);
      return body;
    } catch {
      // fall through: not valid JSON — treat as a raw selector
    }
  }
  return JSON.stringify(body);
}

const _parsedSelectorCache = new Map<string, ParsedSelector>();

function parseSelectorCached(inj: InjectedScript, selectorStr: string): ParsedSelector {
  let parsed = _parsedSelectorCache.get(selectorStr);
  if (parsed === undefined) {
    parsed = inj.parseSelector(selectorStr);
    if (_parsedSelectorCache.size >= 256)
      _parsedSelectorCache.clear();
    _parsedSelectorCache.set(selectorStr, parsed);
  }
  return parsed;
}

function executeSelector(parts: SelectorPart[], root: Node): Element[] {
  const selectorStr = partsToSelectorString(parts);
  try {
    const inj = getInjected();
    const parsed = parseSelectorCached(inj, selectorStr);
    return inj.querySelectorAll(parsed, root);
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

type ElementState = 'visible' | 'hidden' | 'enabled' | 'disabled' | 'editable' | 'stable' | 'stable-frame' | 'checked' | 'unchecked' | 'indeterminate';

/**
 * Whether every `<iframe>` between `node` and the top document is visible
 * — the `frameVisible` Playwright threads through its selector resolution,
 * so an element inside a hidden iframe is not visible either. Same-origin
 * frames only: a cross-origin `frameElement` cannot be read, and is taken
 * as visible.
 */
function frameVisible(node: Node): boolean {
  let win: Window | null = node.ownerDocument?.defaultView ?? null;
  try {
    while (win && win !== win.parent && win.frameElement) {
      if (!isElementVisible(win.frameElement))
        return false;
      win = win.parent;
    }
  } catch {
    // A cross-origin ancestor.
  }
  return true;
}

function isVisible(el: Element): boolean {
  return frameVisible(el) && isElementVisible(el);
}

function elementState(el: Element, state: ElementState): boolean | 'error:notconnected' {
  const result = getInjected().elementState(el, state as any, frameVisible(el));
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
  if (!isVisible(el)) return { actionable: false, reason: 'notvisible' };
  if (getAriaDisabled(el)) return { actionable: false, reason: 'disabled' };
  return { actionable: true };
}

// ── Hit Target Testing (delegates to Playwright) ──

/**
 * Convert a TOP-viewport point (the coordinate space the Rust click
 * path resolves and every backend dispatches in — see
 * `actions.rs::resolve_click_point`, which ADDS the same
 * `frameElement.getBoundingClientRect()` chain) into the coordinate
 * space of `el`'s own document, where `elementsFromPoint` operates.
 * No-op for main-frame elements. Same-origin frames only — a
 * cross-origin iframe has its own engine and an inaccessible
 * `frameElement`, and never composes with a same-document target.
 */
function topViewportPointToLocal(el: Element, p: { x: number; y: number }): { x: number; y: number } {
  let x = p.x;
  let y = p.y;
  let win: Window | null = el.ownerDocument?.defaultView ?? null;
  while (win && win !== win.parent && win.frameElement) {
    const fr = win.frameElement.getBoundingClientRect();
    x -= fr.x;
    y -= fr.y;
    win = win.parent;
  }
  return { x, y };
}

function expectHitTarget(hitPoint: { x: number; y: number }, targetElement: Element): 'done' | { hitTargetDescription: string } {
  return getInjected().expectHitTarget(topViewportPointToLocal(targetElement, hitPoint), targetElement);
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
  const r = getInjected().setupHitTargetInterceptor(el, action, topViewportPointToLocal(el, hitPoint), false);
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
  const inj = getInjected();
  let wantStable = false;
  let inOneFrame = false;
  let nonStable: ElementState[] | null = null;
  for (let i = 0; i < states.length; i++) {
    const s = states[i];
    if (s === 'stable') {
      wantStable = true;
    } else if (s === 'stable-frame') {
      inOneFrame = true;
    } else if (nonStable) {
      nonStable.push(s);
    }
    if ((s === 'stable' || s === 'stable-frame') && !nonStable) {
      nonStable = states.slice(0, i);
    }
  }
  const toCheck = nonStable ?? states;
  if (toCheck.length > 0) {
    const result = await inj.checkElementStates(el, toCheck as any, frameVisible(el));
    if (result === 'error:notconnected') return 'connected';
    if (result !== undefined) return (result as any).missingState;
  }

  if (inOneFrame || wantStable) {
    const playwright = () =>
      inj.checkElementStates(el, ['stable'] as any, frameVisible(el)).then((r: any) => r === undefined);
    const stable = await Promise.race([
      inOneFrame ? stableInOneFrame(el, playwright) : playwright(),
      new Promise<boolean>((resolve) => setTimeout(() => resolve(true), 1000)),
    ]);
    if (!stable) return 'stable';
  }
  return undefined;
}

/**
 * The `stable` state judged over one frame instead of Playwright's two.
 *
 * Playwright takes its first box inside an animation frame and compares
 * at the one after, so a still element costs a frame and whatever was
 * left of the one before. Here the first box is taken at once and
 * compared inside the next frame, which catches the same movers: a frame
 * callback registered before this one has run by then, and a CSS
 * animation or transition has had its clock advanced.
 *
 * What a frame cannot vouch for is an element moved from a timer, which
 * moves whenever the timer says. A frame that lands within
 * `STABLE_WATCH_MS` of the first box is therefore followed by a timer for
 * the rest of it and a last comparison, so the box is always watched for
 * at least that long; Playwright's own watch is one frame interval. A
 * document with an animation in flight gets `playwright` itself: one
 * that started in this very task still reports its starting box.
 */
// Linux WebKit fires a 4ms interval every 8 to 12ms, which 8ms let move
// unseen; much past 12 and a still element costs Firefox a second frame.
const STABLE_WATCH_MS = 12;

function stableInOneFrame(el: Element, playwright: () => Promise<boolean>): Promise<boolean> {
  const inj = getInjected();
  const builtins = inj.utils.builtins;
  const box = () => {
    const target = inj.retarget(el, 'no-follow-label');
    if (!target) return null;
    const rect = target.getBoundingClientRect();
    return [rect.top, rect.left, rect.width, rect.height];
  };
  const first = box();
  if (!first) return Promise.resolve(false);
  const animating = (el.ownerDocument ?? document).getAnimations().some((a) => a.playState === 'running' || a.pending);
  if (animating) return playwright();
  const started = builtins.performance.now();
  return new Promise((resolve) => {
    const unmoved = () => {
      const next = box();
      return next !== null && next.every((value, i) => value === first[i]);
    };
    builtins.requestAnimationFrame(() => {
      if (!unmoved()) return resolve(false);
      const left = STABLE_WATCH_MS - (builtins.performance.now() - started);
      if (left <= 0) return resolve(true);
      builtins.setTimeout(() => resolve(unmoved()), left);
    });
  });
}

/**
 * Combined click pre-flight check, one round-trip instead of three
 * (Playwright's `evaluateInUtility` batching in
 * `dom.ts::_performPointerAction`):
 *
 * 1. `awaitStates` — the requested actionability states.
 * 2. Scroll into view.
 * 3. Iframe-chain accumulated bounding-box → click point.
 *
 * `point` is `null` when the element is not actionable; the host
 * branches on `reason`. Like Playwright, any element may be clicked:
 * a `<select>` takes focus and a file input opens its chooser.
 *
 * With `hitAction`, the hit-target interceptor is armed at the resolved
 * point in the same call (`hit` is `installHitInterceptor`'s answer),
 * which saves the round trip between finding the point and arming it:
 * on a loaded machine that trip alone can cost the action a frame.
 */
async function clickPrep(
  el: Element,
  position: { x: number; y: number } | null,
  states: ElementState[],
  scrollAlign?: ScrollIntoViewOptions | null | false,
  hitAction?: 'hover' | 'tap' | 'mouse' | null,
): Promise<{
  actionable: boolean;
  reason?: string;
  point: { x: number; y: number } | null;
  hit?: string;
}> {
  // Full actionability (visible / enabled / stable as requested) before
  // resolving the click point — mirrors Playwright's `_performPointerAction`
  // order (states gate first, then scroll + clickable point).
  const reason = await awaitStates(el, states);
  if (reason) return { actionable: false, reason, point: null };
  // Scroll into view. On a retry the host passes an explicit alignment
  // (Playwright `_retryPointerAction` cycles undefined/end/center/start to
  // escape position:sticky overlays); otherwise use the precise non-standard
  // `scrollIntoViewIfNeeded`. `false` is the action's `scroll: 'none'`.
  if (scrollAlign === false) {
    // Leave the page where it is.
  } else if (scrollAlign) {
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
    const rects = el.getClientRects();
    for (let i = 0; i < rects.length; i++) {
      const rc = rects[i];
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
      return { actionable: false, reason: 'inviewport', point: null };
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
  const point = { x, y };
  if (hitAction) return { actionable: true, point, hit: installHitInterceptor(el, point, hitAction) };
  return { actionable: true, point };
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
  return getInjected().fill(el, value);
}

function selectOptions(el: Element, ...options: { value?: string; label?: string; index?: number; valueOrLabel?: string }[]): string[] | string {
  return getInjected().selectOptions(el, options);
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
  getInjected().focusNode(el, false);
}

function blurNode(el: Element) {
  getInjected().blurNode(el);
}

function selectText(el: Element) {
  getInjected().selectText(el);
}

function setInputFiles(el: Element, payloads: { name: string; mimeType: string; buffer: string }[]) {
  return getInjected().setInputFiles(el, payloads);
}

function normalizeWS(s: string): string {
  return (s || '').replace(/[\u200b\u00ad]/g, '').trim().replace(/\s+/g, ' ');
}

/**
 * Resolve `parts` to exactly one element and map it, reporting the
 * failure modes as DATA rather than as a thrown error.
 *
 * A thrown message does not survive the round trip: the host sees only
 * `Uncaught`, so a strict-mode violation arrived indistinguishable from
 * any other failure and the match count was lost with it.
 */
function resolveOne<T>(parts: SelectorPart[], map: (el: Element) => T): { ok: T } | { strict: number } | { none: true } {
  const r = executeSelector(parts, document);
  if (r.length === 0)
    return { none: true };
  if (r.length > 1)
    return { strict: r.length };
  return { ok: map(r[0]) };
}

/**
 * Wait in the page for `parts` to reach `state`, for at most `budgetMs`.
 *
 * The host used to ask once and sleep 50ms between asks, so an element
 * that appeared just after an ask was reported up to 50ms late. Here the
 * judgement reruns when the DOM mutates and again every frame, which
 * covers what a mutation record does not: a stylesheet rule inserted
 * through the CSSOM, a transition ending, a shadow root's own subtree.
 *
 * The timers are the builtins captured before page script ran, so an
 * installed fake clock cannot strand the wait. `budgetMs` keeps a wait
 * the host abandoned from outliving it by more than one slice.
 *
 * Answers a JSON string: `{done:true}`, `{done:false}` once the budget
 * is spent, `{strict:n}`, or `{error}`.
 */
function waitForState(
  parts: SelectorPart[],
  strict: boolean,
  state: 'attached' | 'detached' | 'visible' | 'hidden',
  budgetMs: number,
): Promise<string> {
  const builtins = getInjected().utils.builtins;
  return new Promise(resolve => {
    let settled = false;
    // A selector that is expensive to run is judged once a frame rather
    // than once per batch of mutations.
    let costly = false;
    const observer = new MutationObserver(() => {
      if (!costly) judge();
    });
    const finish = (answer: string) => {
      if (settled) return;
      settled = true;
      observer.disconnect();
      resolve(answer);
    };
    const judge = () => {
      if (settled) return;
      const started = builtins.performance.now();
      try {
        const found = executeSelector(parts, document);
        if (strict && found.length > 1) {
          finish(JSON.stringify({ strict: found.length }));
          return;
        }
        const attached = found.length > 0;
        const visible = () => elementState(found[0], 'visible') === true;
        const reached = state === 'attached' ? attached
          : state === 'detached' ? !attached
            : state === 'visible' ? attached && visible()
              : !attached || !visible();
        if (reached) finish('{"done":true}');
      } catch (e) {
        finish(JSON.stringify({ error: String(e) }));
      }
      costly = builtins.performance.now() - started > 2;
    };
    const frame = () => {
      judge();
      if (!settled) builtins.requestAnimationFrame(frame);
    };
    judge();
    if (settled) return;
    observer.observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
    builtins.requestAnimationFrame(frame);
    builtins.setTimeout(() => finish('{"done":false}'), budgetMs);
  });
}

/**
 * Resolve when the document changes, or after `budgetMs` with `false`.
 *
 * A retry that sleeps its whole backoff judges a change late by however
 * much of the sleep was left. The host parks here instead, so the sleep
 * ends with the change that could alter the verdict. What a mutation
 * record does not carry reaches it as the event the change raises: a
 * typed value, a toggled control, focus moving, an animation ending, a
 * resource arriving. A change that raises neither (a shadow root's own
 * subtree, a property set from script) is seen when the budget ends,
 * which is when the plain sleep would have looked.
 */
const CHANGE_EVENTS = ['input', 'change', 'focusin', 'focusout', 'transitionend', 'animationend', 'load', 'error'];

function waitForChange(budgetMs: number): Promise<boolean> {
  const builtins = (window as any).__pwClock?.builtins ?? initialBuiltins;
  return new Promise(resolve => {
    let settled = false;
    const finish = (changed: boolean) => {
      if (settled) return;
      settled = true;
      observer.disconnect();
      for (const type of CHANGE_EVENTS) document.removeEventListener(type, onEvent, true);
      resolve(changed);
    };
    const onEvent = () => finish(true);
    const observer = new MutationObserver(onEvent);
    observer.observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
    for (const type of CHANGE_EVENTS) document.addEventListener(type, onEvent, true);
    builtins.setTimeout(() => finish(false), budgetMs);
  });
}

/**
 * Count requests that are STILL IN FLIGHT.
 *
 * `performance.getEntriesByType('resource')` only gains an entry once a
 * request finishes, so a pending `fetch` is invisible to it and
 * `networkidle` fired while an action's XHR was still running. Wrapping
 * the two APIs a page actually starts work with is what makes the state
 * mean what it says.
 *
 * Installed at document start, before page script runs, so a fetch
 * issued from an inline script is counted.
 */
function installInFlightCounter(): void {
  const w = window as any;
  if (w.__fd_inflight !== undefined)
    return;
  w.__fd_inflight = 0;

  const originalFetch = w.fetch;
  if (typeof originalFetch === 'function') {
    w.fetch = function(...args: unknown[]) {
      w.__fd_inflight++;
      // `finally` and not `then`: a rejected fetch has also stopped
      // occupying the network, and leaving the count raised would mean
      // the page never goes idle again.
      return originalFetch.apply(this, args).finally(() => { w.__fd_inflight--; });
    };
  }

  const XHR = w.XMLHttpRequest;
  if (typeof XHR === 'function' && XHR.prototype && typeof XHR.prototype.send === 'function') {
    const originalSend = XHR.prototype.send;
    XHR.prototype.send = function(this: XMLHttpRequest, ...args: unknown[]) {
      w.__fd_inflight++;
      let settled = false;
      const done = () => {
        if (settled)
          return;
        settled = true;
        w.__fd_inflight--;
      };
      this.addEventListener('loadend', done);
      try {
        return originalSend.apply(this, args);
      } catch (e) {
        // A synchronous throw means the request never started.
        done();
        throw e;
      }
    };
  }
}

installInFlightCounter();

// ── Install on window.__fd ──

declare global {
  interface Window { __fd: any; }
}

let lastCheckedEl: Element | null = null;

function checkedOf(node: Node): boolean | 'error' {
  const el = getInjected().retarget(node, 'follow-label');
  return el ? getCheckedWithoutMixed(el) : 'error';
}

async function prepElementForPointer(
  el: Element,
  position: { x: number; y: number } | null,
  states: ElementState[],
  scrollAlign: ScrollIntoViewOptions | null | false | undefined,
  hitAction: 'hover' | 'tap' | 'mouse' | 'drag' | null | undefined,
  force: boolean,
  trial: boolean,
): Promise<{
  actionable: boolean;
  reason?: string;
  point: { x: number; y: number } | null;
  hit?: string;
}> {
  if (force) {
    if (scrollAlign === false) {
      // Leave the page where it is.
    } else if (scrollAlign) {
      el.scrollIntoView(scrollAlign);
    } else if (typeof (el as any).scrollIntoViewIfNeeded === 'function') {
      (el as any).scrollIntoViewIfNeeded();
    } else {
      el.scrollIntoView({ block: 'center', inline: 'center' });
    }
    const r = el.getBoundingClientRect();
    let x = position ? r.x + position.x : r.x + r.width / 2;
    let y = position ? r.y + position.y : r.y + r.height / 2;
    if (!position && r.width * r.height > 0) {
      const vw = window.innerWidth;
      const vh = window.innerHeight;
      const left = Math.max(r.left, 0);
      const right = Math.min(r.right, vw);
      const top = Math.max(r.top, 0);
      const bottom = Math.min(r.bottom, vh);
      if ((right - left) * (bottom - top) <= 0.99 || right <= left || bottom <= top) {
        return { actionable: false, reason: 'inviewport', point: null };
      }
      x = (left + right) / 2;
      y = (top + bottom) / 2;
    }
    let win: any = (el.ownerDocument as Document).defaultView;
    while (win && win !== win.parent && win.frameElement) {
      const fr = (win.frameElement as Element).getBoundingClientRect();
      x += fr.x;
      y += fr.y;
      win = win.parent;
    }
    return { actionable: true, point: { x, y } };
  }
  const res = await clickPrep(el, position, states, scrollAlign, trial ? null : (hitAction as any));
  if (!res.actionable || !trial || !res.point) return res;
  const h = expectHitTarget(res.point, el);
  return { ...res, hit: h === 'done' ? 'ok' : h.hitTargetDescription };
}

async function evalExpectOnce(parts: SelectorPart[], params: any, strict: boolean): Promise<{ settled: boolean; json: string }> {
  try {
    const isArray = params.expression === 'to.have.count' || params.expression.endsWith('.array');
    const elements = executeSelector(parts, document);
    if (strict && !isArray && elements.length > 1)
      return { settled: true, json: JSON.stringify({ strict: elements.length }) };
    if (!elements.length) {
      const expr = params.expression;
      const isNot = Boolean(params.isNot);
      let matches = isNot;
      if (expr === 'to.have.count') matches = params.expectedNumber === 0;
      else if (isArray) matches = !params.expectedText || params.expectedText.length === 0;
      else if (!isNot && (expr === 'to.be.hidden' || expr === 'to.be.detached')) matches = true;
      else if (isNot && (expr === 'to.be.visible' || expr === 'to.be.attached' || expr === 'to.be.in.viewport')) matches = false;
      return { settled: matches !== isNot, json: JSON.stringify({ missing: true }) };
    }
    const inj = getInjected();
    const log = isArray
      ? `  locator resolved to ${elements.length} element${elements.length === 1 ? '' : 's'}`
      : `  locator resolved to ${inj.previewNode(elements[0])}`;
    const result = await inj.expect(elements[0], params, elements, frameVisible(elements[0]));
    const passed = Boolean(result.matches) !== Boolean(params.isNot);
    return { settled: passed, json: JSON.stringify({ ...result, log }) };
  } catch (e: any) {
    return { settled: true, json: JSON.stringify({ error: e.message }) };
  }
}

function expectWait(parts: SelectorPart[], params: any, strict: boolean, budgetMs: number): Promise<string> {
  const builtins = (window as any).__pwClock?.builtins ?? initialBuiltins;
  return new Promise(async resolve => {
    let last = await evalExpectOnce(parts, params, strict);
    if (last.settled || budgetMs <= 0) {
      resolve(last.json);
      return;
    }
    let done = false;
    let running = false;
    let rerun = false;
    let costly = false;
    const finish = (json: string) => {
      if (done) return;
      done = true;
      observer.disconnect();
      for (const type of CHANGE_EVENTS) document.removeEventListener(type, trigger, true);
      resolve(json);
    };
    const judge = async () => {
      if (done) return;
      if (running) {
        rerun = true;
        return;
      }
      running = true;
      const started = builtins.performance.now();
      try {
        last = await evalExpectOnce(parts, params, strict);
        if (last.settled) {
          finish(last.json);
          return;
        }
      } finally {
        running = false;
        costly = builtins.performance.now() - started > 2;
      }
      if (rerun && !done) {
        rerun = false;
        void judge();
      }
    };
    const trigger = () => {
      if (!costly) void judge();
    };
    const observer = new MutationObserver(trigger);
    observer.observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
    for (const type of CHANGE_EVENTS) document.addEventListener(type, trigger, true);
    const frame = () => {
      if (done) return;
      void judge();
      builtins.requestAnimationFrame(frame);
    };
    builtins.requestAnimationFrame(frame);
    builtins.setTimeout(() => finish(last.json), budgetMs);
  });
}

// A `window.__fd` from another realm (an injection that ran in the
// outgoing document) is replaced: its promises can never settle.
if (!(window.__fd instanceof Object)) {
  window.__fd = {
    // Playwright InjectedScript instance (constructed lazily on first access)
    get _injected() { return getInjected(); },

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

    /**
     * One judgement of a web-first assertion: the page half of
     * Playwright's `frames.ts::_expectInternal`. Single-element
     * expressions are strict when `strict` is set; array expressions see
     * every match. The answer is a JSON string so a strict breach, an
     * engine error and an arbitrary received value all cross every
     * backend as data rather than as a thrown, backend-mangled message.
     */
    async expect(parts: SelectorPart[], params: any, strict: boolean) {
      return (await evalExpectOnce(parts, params, strict)).json;
    },
    expectWait,
    ariaSnapshotForExpectFailure(parts?: SelectorPart[], params?: any) {
      if (!((window as any).__fdAria instanceof Object))
        return null;
      const inj = getInjected();
      if (parts && params) {
        const els = executeSelector(parts, document);
        const el = els.length > 0 ? els[0] : undefined;
        return inj.ariaSnapshotForExpect(el, params, el ? frameVisible(el) : false) ?? null;
      }
      return document.body ? inj.ariaSnapshotForExpectFailure(document.body, { mode: 'default' }) : null;
    },

    /**
     * Resolve `parts` and run `fill` in a single round-trip (designed to execute
     * directly inside `UTILITY_WORLD`, avoiding RemoteObjectId + DOM.describeNode
     * + DOM.resolveNode + Runtime.releaseObject).
     */
    resolveAndFill(parts: SelectorPart[], strict: boolean, value: string, force: boolean): string {
      try {
        const r = executeSelector(parts, document);
        if (strict && r.length > 1) return JSON.stringify({ strict: r.length });
        if (!r.length) return JSON.stringify({ result: 'error:notfound' });
        const el = r[0];
        if (!force) {
          const state = checkElementStates(el, ['visible', 'enabled', 'editable']);
          if (state !== 'done') return JSON.stringify({ result: state });
        }
        return JSON.stringify({ result: fillElement(el, value) });
      } catch (e: any) {
        return JSON.stringify({ error: String(e && e.message || e) });
      }
    },

    /**
     * Resolve `parts` + run pointer actionability, scroll, point calculation,
     * and hit-target interceptor setup in a single `Runtime.evaluate`.
     */
    async resolveAndClickPrep(
      parts: SelectorPart[],
      strict: boolean,
      position: { x: number; y: number } | null,
      states: ElementState[],
      scrollAlign?: ScrollIntoViewOptions | null | false,
      hitAction?: 'hover' | 'tap' | 'mouse' | 'drag' | null,
      force?: boolean,
      trial?: boolean,
    ): Promise<string> {
      try {
        const r = executeSelector(parts, document);
        if (strict && r.length > 1) return JSON.stringify({ strict: r.length });
        if (!r.length) return JSON.stringify({ actionable: false, reason: 'notfound', point: null });
        const res = await prepElementForPointer(r[0], position, states, scrollAlign, hitAction, !!force, !!trial);
        return JSON.stringify(res);
      } catch (e: any) {
        return JSON.stringify({ error: String(e && e.message || e) });
      }
    },

    /**
     * Resolve `parts` + check current checked/radio state + run pointer prep
     * in a single `Runtime.evaluate`. Stashes the matched element for
     * [`finalizeCheck`] to verify the post-click state alongside hit teardown.
     */
    async resolveAndCheckPrep(
      parts: SelectorPart[],
      strict: boolean,
      targetChecked: boolean,
      position: { x: number; y: number } | null,
      states: ElementState[],
      scrollAlign?: ScrollIntoViewOptions | null | false,
      hitAction?: 'hover' | 'tap' | 'mouse' | 'drag' | null,
      force?: boolean,
      trial?: boolean,
    ): Promise<string> {
      try {
        const r = executeSelector(parts, document);
        if (strict && r.length > 1) return JSON.stringify({ strict: r.length });
        if (!r.length) return JSON.stringify({ actionable: false, reason: 'notfound', point: null });
        const el = r[0];
        const current = checkedOf(el);
        const isRadio = el.nodeName === 'INPUT' && (el as HTMLInputElement).type === 'radio';
        if (typeof current !== 'boolean') {
          return JSON.stringify({ invalid: 'not a checkbox, radio button, or ARIA-checkable element' });
        }
        if (current === targetChecked) {
          return JSON.stringify({ alreadyDone: true });
        }
        if (!targetChecked && isRadio) {
          return JSON.stringify({
            invalid: 'Cannot uncheck radio button. Radio buttons can only be unchecked by selecting another radio button in the same group.',
          });
        }
        lastCheckedEl = el;
        const res = await prepElementForPointer(el, position, states, scrollAlign, hitAction, !!force, !!trial);
        return JSON.stringify(res);
      } catch (e: any) {
        return JSON.stringify({ error: String(e && e.message || e) });
      }
    },

    finalizeCheck(targetChecked: boolean, finalizeHit: boolean): string {
      const el = lastCheckedEl;
      lastCheckedEl = null;
      if (finalizeHit) {
        const hit = finalizeHitInterceptor();
        if (hit !== 'done') {
          return JSON.stringify({ hit });
        }
      }
      const cur = el ? checkedOf(el) : 'error';
      return JSON.stringify({ hit: 'done', changed: cur === targetChecked });
    },

    /**
     * Paint `color` over every element a screenshot `mask` selector
     * resolves to, tagging each overlay with `tag` so the teardown can
     * remove exactly what it added. Mirrors Playwright's
     * `injected.addMaskedElements(elements, color)`, driven from
     * `server/screenshotter.ts::_maskElements`.
     *
     * Resolution goes through the selector engine, not
     * `document.querySelectorAll`: a mask takes Locators, so `getByRole`,
     * `internal:has-text`, `xpath=` and every other engine has to resolve
     * here. A DOM-only query silently matched nothing for all of them and
     * the capture came back unmasked.
     */
    addMaskedElements(selector: string, color: string, tag: string) {
      let elements: Element[] = [];
      try {
        const inj = getInjected();
        elements = inj.querySelectorAll(inj.parseSelector(selector), document);
      } catch {
        try { elements = [...document.querySelectorAll(selector)]; } catch { return; }
      }
      for (const el of elements) {
        const r = el.getBoundingClientRect();
        const o = document.createElement('div');
        o.className = tag;
        o.style.cssText = `all: initial; position: fixed; left: ${r.left}px; top: ${r.top}px; width: ${r.width}px; height: ${r.height}px; background: ${color}; z-index: 2147483647; pointer-events: none;`;
        document.body.appendChild(o);
      }
    },
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
      return JSON.stringify(resolveOne(parts, el => getInjected().generateSelectorSimple(el)));
    },

    /**
     * As `normalizeSelector`, but returns the locator EXPRESSION a
     * developer would paste into a test rather than the selector string.
     * `page.getByRole('button', { name: 'Sign in' })` instead of
     * `internal:role=button[name="Sign in"s]`.
     *
     * Mirrors Playwright's `playwright.locator(element)` console helper
     * (`consoleApi.ts::_generateLocator`), which is the same
     * `generateSelectorSimple` result passed through `asLocator`.
     */
    generateLocator(parts: SelectorPart[], language?: Language): string {
      return JSON.stringify(
        resolveOne(parts, el => {
          const inj = getInjected();
          return inj.utils.asLocator(language || 'javascript', inj.generateSelectorSimple(el));
        }),
      );
    },

    // Playwright selector API (direct access)
    parseSelector: (s: string) => getInjected().parseSelector(s),
    querySelector: (selector: any, root: Node, strict: boolean) => getInjected().querySelector(selector, root, strict),
    querySelectorAll: (selector: any, root: Node) => getInjected().querySelectorAll(selector, root),

    // The element highlights this frame shows: the whole set, every time
    // (`injected.setHighlights`, driven by the host's highlight controller
    // as Playwright's `HighlightController` drives it). `parts` is
    // ferridriver's {engine, body} array, converted to a ParsedSelector so
    // the overlay re-resolves it as the DOM changes; `cssStyle` is already
    // a CSS string.
    setHighlights(entries: { parts: SelectorPart[]; cssStyle?: string }[]) {
      const inj = getInjected();
      inj.setHighlights(entries.map(e => ({
        selector: inj.parseSelector(partsToSelectorString(e.parts)),
        cssStyle: e.cssStyle,
      })));
    },
    hideHighlight() {
      if (_injected) _injected.hideHighlight();
    },

    // Actionability
    elementState,
    checkElementStates,
    isActionable,
    isVisible,
    expectHitTarget,
    installHitInterceptor,
    finalizeHitInterceptor,

    // Actions (Playwright-ported)
    clearAndDispatch,
    dispatchInputEvents,
    clickGuard,
    clickPrep,
    awaitStates,
    waitForState,
    waitForChange,
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
    getAccessibleName: getElementAccessibleNameText,
    getAriaDisabled,
    // Playwright reads the checked state AFTER retargeting through
    // `follow-label` (`injectedScript.ts::elementState`), so a locator
    // that lands on a `<label>` (or on text inside one) reports the
    // state of the control the label owns. Returns `'error'` for
    // anything that is neither an input checkbox/radio nor an
    // `aria-checked` role, exactly like `getCheckedWithoutMixed`.
    getChecked: (node: Node) => checkedOf(node),
    // Playwright's `frames.ts::inputValue`: a label reads the control it
    // owns, and anything else is an error rather than `undefined`.
    inputValue: (node: Node) => {
      const el = getInjected().retarget(node, 'follow-label');
      if (!el || (el.nodeName !== 'INPUT' && el.nodeName !== 'TEXTAREA' && el.nodeName !== 'SELECT'))
        throw new Error('Node is not an <input>, <textarea> or <select> element');
      return (el as HTMLInputElement).value;
    },
    getReadonly,

    // Playwright's aria snapshot (locator/page scoped). `node` is the
    // root resolved by Rust (strict resolution + auto-wait done
    // host-side — source of truth); delegates to the vendored
    // InjectedScript so the rendered YAML is byte-for-byte Playwright.
    // Playwright's `to.match.aria`: a PARSED `AriaTemplateNode` matched
    // against the live tree, containment semantics and all
    // (`ariaSnapshot.ts::matchesExpectAriaTemplate`). The YAML -> template
    // parse is not here — it needs the `yaml` library, so it lives in the
    // on-demand `aria-support` bundle exactly like Playwright keeps it
    // out of the injected script.
    matchAriaTemplate: (node: Element, template: any) => {
      const result = (window as any).__fdAria.matchesExpectAriaTemplate(node, template);
      return { matches: result.matches.length > 0, received: result.received.raw, regex: result.received.regex };
    },
    ariaSnapshot: (node: Node, options?: { mode?: 'ai' | 'default'; depth?: number }) =>
      getInjected().ariaSnapshot(node, { mode: options?.mode || 'default', depth: options?.depth }),
    // One frame's tree plus the `iframeRefs` / `iframeDepths` the Rust
    // core needs to merge child-iframe trees into it (server
    // `ariaSnapshotJSONForFrame`). The merged tree renders once, through
    // `renderAriaSnapshot`, as Playwright renders it.
    ariaSnapshotFrame: (
      node: Node,
      options?: { mode?: 'ai' | 'default'; depth?: number; boxes?: boolean },
    ) => {
      const { json, iframeRefs, iframeDepths } = getInjected().ariaSnapshotJSON(node, {
        mode: options?.mode || 'default',
        depth: options?.depth,
        boxes: options?.boxes,
      });
      return { json, iframeRefs, iframeDepths };
    },
    renderAriaSnapshot: (json: any) => (window as any).__fdAria.renderAriaSnapshotAsYaml(json),
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
