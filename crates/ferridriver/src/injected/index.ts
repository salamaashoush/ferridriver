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
  stableRafCount: 2,
  browserName: 'chromium',
  customEngines: [],
});

// ── Selector Execution (compatibility with ferridriver's parts-based API) ──

function executeSelector(parts: SelectorPart[], root: Node): Element[] {
  // Convert ferridriver's {engine, body} parts to Playwright's parsed selector format
  // Map ferridriver engine names to Playwright engine names
  // Build the selector string using Playwright's own escaping conventions.
  // body format from Rust: raw text for text/label, attribute value for placeholder/alt/title,
  // role spec for role, CSS for css, etc.
  // exact flag: body enclosed in double quotes = exact match, otherwise substring/case-insensitive.
  const selectorStr = parts.map(p => {
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
function clickPrep(
  el: Element,
  position: { x: number; y: number } | null,
): {
  guard: string;
  actionable: boolean;
  reason?: string;
  point: { x: number; y: number } | null;
} {
  const guard = clickGuard(el);
  if (guard) return { guard, actionable: false, point: null };
  const act = isActionable(el);
  if (!act.actionable) {
    return { guard: '', actionable: false, reason: act.reason, point: null };
  }
  if (typeof (el as any).scrollIntoViewIfNeeded === 'function') {
    (el as any).scrollIntoViewIfNeeded();
  } else {
    el.scrollIntoView({ block: 'center', inline: 'center' });
  }
  const r = el.getBoundingClientRect();
  let x = position ? r.x + position.x : r.x + r.width / 2;
  let y = position ? r.y + position.y : r.y + r.height / 2;
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

// ── Utility helpers ──

function normalizeWS(s: string): string {
  return (s || '').replace(/[\u200b\u00ad]/g, '').trim().replace(/\s+/g, ' ');
}

function allElements(root?: Node): Element[] {
  root = root || document;
  const out: Element[] = [];
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
  while (walker.nextNode()) {
    const el = walker.currentNode as Element;
    out.push(el);
    if (el.shadowRoot) {
      const shadowEls = allElements(el.shadowRoot);
      out.push(...shadowEls);
    }
  }
  return out;
}

function searchPage(pattern: string, isRegex: boolean, caseSensitive: boolean, contextChars: number, cssScope: string, maxResults: number) {
  let regex: RegExp;
  try {
    regex = isRegex ? new RegExp(pattern, caseSensitive ? 'g' : 'gi') : new RegExp(pattern.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), caseSensitive ? 'g' : 'gi');
  } catch { return JSON.stringify({ error: 'Invalid pattern' }); }
  const scope = cssScope ? document.querySelector(cssScope) || document.body : document.body;
  const text = scope.innerText || '';
  const matches: { match_text: string; context: string; element_path: string; char_position: number }[] = [];
  let m;
  while ((m = regex.exec(text)) && matches.length < maxResults) {
    const start = Math.max(0, m.index - contextChars);
    const end = Math.min(text.length, m.index + m[0].length + contextChars);
    matches.push({ match_text: m[0], context: text.slice(start, end), element_path: '', char_position: m.index });
  }
  return JSON.stringify({ total: matches.length, has_more: false, matches });
}

function findElementsCSS(selector: string, attributes: string[], maxResults: number, includeText: boolean) {
  try {
    const elements = [...document.querySelectorAll(selector)].slice(0, maxResults);
    return JSON.stringify(elements.map((el, i) => {
      const item: Record<string, string | number> = { index: i, tag: el.tagName.toLowerCase() };
      for (const attr of attributes) {
        const val = el.getAttribute(attr);
        if (val !== null) item[attr] = val;
      }
      if (includeText) item.text = (el.textContent || '').trim().slice(0, 200);
      return item;
    }));
  } catch (e: any) { return JSON.stringify({ error: e.message }); }
}

function scrollInfo() {
  return JSON.stringify({ scrollY: window.scrollY, scrollHeight: document.documentElement.scrollHeight, viewportHeight: window.innerHeight });
}

function suggestSelectors() {
  const ids = [...document.querySelectorAll('[id]')].map(el => el.id).filter(Boolean).slice(0, 50);
  const inputs = [...document.querySelectorAll('input,textarea,select,button,a')].map(el => {
    const tag = el.tagName.toLowerCase();
    const id = el.id ? `#${el.id}` : '';
    const name = el.getAttribute('name') ? `[name="${el.getAttribute('name')}"]` : '';
    const type = el.getAttribute('type') ? `[type="${el.getAttribute('type')}"]` : '';
    return `${tag}${id}${name}${type}`;
  }).slice(0, 50);
  return JSON.stringify({ ids, inputs });
}

let consoleErrorCount = 0;
let consoleErrorsInstalled = false;
function consoleErrors(): number {
  if (!consoleErrorsInstalled) {
    consoleErrorsInstalled = true;
    const origError = console.error;
    console.error = function (...args: any[]) {
      consoleErrorCount++;
      origError.apply(console, args);
    };
  }
  return consoleErrorCount;
}

function waitForActionable(el: Element, timeoutMs: number): Promise<string> {
  return new Promise((resolve) => {
    const deadline = Date.now() + timeoutMs;
    let lastRect: DOMRect | null = null;
    let stableCount = 0;
    function check() {
      if (Date.now() >= deadline) { resolve('timeout'); return; }
      if (!el.isConnected) { resolve('detached'); return; }
      if (!isElementVisible(el)) { setTimeout(check, 50); return; }
      if (getAriaDisabled(el)) { setTimeout(check, 50); return; }
      const rect = el.getBoundingClientRect();
      if (lastRect && rect.x === lastRect.x && rect.y === lastRect.y && rect.width === lastRect.width && rect.height === lastRect.height) {
        if (++stableCount >= 2) { resolve('done'); return; }
      } else { stableCount = 0; }
      lastRect = rect;
      requestAnimationFrame(check);
    }
    requestAnimationFrame(check);
  });
}

function extractMarkdown(): string {
  const clone = document.body.cloneNode(true) as HTMLElement;
  clone.querySelectorAll('script,style,noscript,svg,iframe').forEach(el => el.remove());
  function walk(node: Node): string {
    if (node.nodeType === 3) return (node.textContent || '').replace(/\s+/g, ' ');
    if (node.nodeType !== 1) return '';
    const el = node as HTMLElement;
    const tag = el.tagName.toLowerCase();
    const children = [...el.childNodes].map(walk).join('');
    if (/^h[1-6]$/.test(tag)) return '\n' + '#'.repeat(parseInt(tag[1])) + ' ' + children.trim() + '\n';
    if (tag === 'p' || tag === 'div') return '\n' + children.trim() + '\n';
    if (tag === 'br') return '\n';
    if (tag === 'li') return '- ' + children.trim() + '\n';
    if (tag === 'a') return `[${children.trim()}](${el.getAttribute('href') || ''})`;
    if (tag === 'strong' || tag === 'b') return `**${children.trim()}**`;
    if (tag === 'em' || tag === 'i') return `*${children.trim()}*`;
    if (tag === 'code') return `\`${children.trim()}\``;
    if (tag === 'pre') return '\n```\n' + children.trim() + '\n```\n';
    if (tag === 'img') return `![${el.getAttribute('alt') || ''}](${el.getAttribute('src') || ''})`;
    return children;
  }
  return walk(clone).replace(/\n{3,}/g, '\n\n').trim();
}

function dismissDialogs() {
  window.alert = () => {};
  window.confirm = () => true;
  window.prompt = () => '';
}

// ── Accessibility Tree (shared by BiDi + WebKit backends) ──

interface AXNode {
  nodeId: string;
  parentId: string | null;
  backendId: number;
  role: string;
  name: string;
  ignored: boolean;
  description: string;
  checked: string;
  disabled: boolean;
  readonly: boolean;
  level: number;
  valueMin: number;
  valueMax: number;
  valueNow: number;
  valueText: string;
  expanded: string;
  selected: boolean;
  required: boolean;
  url: string;
  keyShortcuts: string;
}

function accessibilityTree(maxDepth: number): AXNode[] {
  // Clear previous ref tags
  document.querySelectorAll('[data-fdref]').forEach(e => e.removeAttribute('data-fdref'));

  const nodes: AXNode[] = [];
  let nextId = 0;

  function walk(el: Element, parentId: string | null, depth: number) {
    if (maxDepth >= 0 && depth > maxDepth) return;

    const nid = nextId++;
    const nodeId = String(nid);

    // Tag the element for later resolution via CSS attribute selector
    if (el.setAttribute) el.setAttribute('data-fdref', nodeId);

    // Compute ARIA role using Playwright's role computation
    let role = '';
    try { role = getAriaRole(el) || ''; } catch { /* noop */ }
    if (!role) {
      // Fallback: map common tags
      const tag = el.tagName;
      const tagRoles: Record<string, string> = {
        A: 'link', NAV: 'navigation', MAIN: 'main', HEADER: 'banner',
        FOOTER: 'contentinfo', ASIDE: 'complementary', SECTION: 'region',
        ARTICLE: 'article', FORM: 'form', TABLE: 'table', THEAD: 'rowgroup',
        TBODY: 'rowgroup', TR: 'row', TH: 'columnheader', TD: 'cell',
        UL: 'list', OL: 'list', LI: 'listitem', DL: 'list', DT: 'term',
        DD: 'definition', DIALOG: 'dialog', DETAILS: 'group',
        PROGRESS: 'progressbar', METER: 'meter', OUTPUT: 'status',
        HR: 'separator', IMG: 'img', FIGURE: 'figure',
        BLOCKQUOTE: 'blockquote', PRE: 'generic', CODE: 'code',
      };
      role = tagRoles[tag] || '';
    }

    // Skip noise nodes (generic, no role), but still walk children
    if (!role || role === 'none' || role === 'presentation') {
      for (const child of el.children) walk(child as Element, parentId, depth);
      if (el.shadowRoot) {
        for (const child of el.shadowRoot.children) walk(child as Element, parentId, depth);
      }
      return;
    }

    // Compute accessible name using Playwright's name computation
    let name = '';
    try { name = getElementAccessibleName(el, false) || ''; } catch { /* noop */ }
    if (!name) {
      // Fallback for common cases
      name = el.getAttribute?.('aria-label')
        || el.getAttribute?.('alt')
        || el.getAttribute?.('title')
        || el.getAttribute?.('placeholder')
        || '';
    }
    // If still no name, use trimmed text content (capped)
    if (!name && el.textContent) {
      name = el.textContent.trim().substring(0, 100);
    }

    // Build extra properties
    const htmlEl = el as HTMLElement;
    const inputEl = el as HTMLInputElement;
    let description = el.getAttribute?.('aria-description') || '';
    let checked = '';
    try {
      const c = getCheckedWithoutMixed(el);
      if (c === true) checked = 'true';
      else if (c === false) checked = 'false';
    } catch { /* noop */ }
    const disabled = !!getAriaDisabled(el);
    let readonlyVal = false;
    try { readonlyVal = getReadonly(el); } catch { /* noop */ }

    // Heading level
    let level = 0;
    const tag = el.tagName;
    if (/^H[1-6]$/.test(tag)) level = parseInt(tag[1]);
    const ariaLevel = el.getAttribute?.('aria-level');
    if (ariaLevel) level = parseInt(ariaLevel) || level;

    // Value properties (for range inputs, progress, etc.)
    let valueMin = 0, valueMax = 0, valueNow = 0, valueText = '';
    if ('valueAsNumber' in el) {
      valueNow = inputEl.valueAsNumber || 0;
      valueMin = parseFloat(inputEl.min) || 0;
      valueMax = parseFloat(inputEl.max) || 100;
    }
    const ariaValueNow = el.getAttribute?.('aria-valuenow');
    if (ariaValueNow) valueNow = parseFloat(ariaValueNow) || 0;
    const ariaValueText = el.getAttribute?.('aria-valuetext');
    if (ariaValueText) valueText = ariaValueText;

    // Expanded state
    let expanded = '';
    const ariaExpanded = el.getAttribute?.('aria-expanded');
    if (ariaExpanded === 'true') expanded = 'true';
    else if (ariaExpanded === 'false') expanded = 'false';

    const selected = el.getAttribute?.('aria-selected') === 'true'
      || (el as HTMLOptionElement).selected === true;
    const required = el.getAttribute?.('aria-required') === 'true'
      || inputEl.required === true;
    const url = (el as HTMLAnchorElement).href || el.getAttribute?.('href') || '';
    const keyShortcuts = el.getAttribute?.('aria-keyshortcuts') || '';

    nodes.push({
      nodeId, parentId, backendId: nid, role, name, ignored: false,
      description, checked, disabled, readonly: readonlyVal, level,
      valueMin, valueMax, valueNow, valueText, expanded, selected,
      required, url, keyShortcuts,
    });

    // Recurse into children
    if (el.shadowRoot) {
      for (const child of el.shadowRoot.children) walk(child as Element, nodeId, depth + 1);
    }
    for (const child of el.children) walk(child as Element, nodeId, depth + 1);
  }

  walk(document.documentElement, null, 0);
  return nodes;
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

    // Playwright selector API (direct access)
    parseSelector: (s: string) => injected.parseSelector(s),
    querySelector: (selector: any, root: Node, strict: boolean) => injected.querySelector(selector, root, strict),
    querySelectorAll: (selector: any, root: Node) => injected.querySelectorAll(selector, root),

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
    selectOption,
    selectOptions,
    getOptions,
    fill: fillElement,
    focusNode,
    blurNode,
    selectText,
    setInputFiles,

    // Utilities
    searchPage,
    findElementsCSS,
    scrollInfo,
    suggestSelectors,
    consoleErrors,
    waitForActionable,
    extractMarkdown,
    dismissDialogs,
    allElements,

    // ARIA
    getAriaRole,
    getAccessibleName: getElementAccessibleName,
    getAriaDisabled,
    getChecked: getCheckedWithoutMixed,
    getReadonly,

    // Accessibility tree (shared by BiDi + WebKit)
    accessibilityTree,

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
      options?: { mode?: 'ai' | 'default'; depth?: number; refPrefix?: string },
    ) =>
      injected.incrementalAriaSnapshot(node, {
        mode: options?.mode || 'default',
        depth: options?.depth,
        refPrefix: options?.refPrefix,
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
