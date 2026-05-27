import { isElementVisible } from './domUtils';
import { getAriaDisabled, getAriaRole, getCheckedWithoutMixed, getElementAccessibleName, getReadonly } from './roleUtils';

declare global {
  interface Window { __fd: any; }
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
        if (val !== null)
          item[attr] = val;
      }
      if (includeText)
        item.text = (el.textContent || '').trim().slice(0, 200);
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
    console.error = function(...args: any[]) {
      consoleErrorCount++;
      origError.apply(console, args);
    };
  }
  return consoleErrorCount;
}

function waitForActionable(el: Element, timeoutMs: number): Promise<string> {
  return new Promise(resolve => {
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
      } else {
        stableCount = 0;
      }
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
    if (node.nodeType === 3)
      return (node.textContent || '').replace(/\s+/g, ' ');
    if (node.nodeType !== 1)
      return '';
    const el = node as HTMLElement;
    const tag = el.tagName.toLowerCase();
    const children = [...el.childNodes].map(walk).join('');
    if (/^h[1-6]$/.test(tag))
      return '\n' + '#'.repeat(parseInt(tag[1])) + ' ' + children.trim() + '\n';
    if (tag === 'p' || tag === 'div')
      return '\n' + children.trim() + '\n';
    if (tag === 'br')
      return '\n';
    if (tag === 'li')
      return '- ' + children.trim() + '\n';
    if (tag === 'a')
      return `[${children.trim()}](${el.getAttribute('href') || ''})`;
    if (tag === 'strong' || tag === 'b')
      return `**${children.trim()}**`;
    if (tag === 'em' || tag === 'i')
      return `*${children.trim()}*`;
    if (tag === 'code')
      return `\`${children.trim()}\``;
    if (tag === 'pre')
      return '\n```\n' + children.trim() + '\n```\n';
    if (tag === 'img')
      return `![${el.getAttribute('alt') || ''}](${el.getAttribute('src') || ''})`;
    return children;
  }
  return walk(clone).replace(/\n{3,}/g, '\n\n').trim();
}

function dismissDialogs() {
  window.alert = () => {};
  window.confirm = () => true;
  window.prompt = () => '';
}

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
  document.querySelectorAll('[data-fdref]').forEach(e => e.removeAttribute('data-fdref'));

  const nodes: AXNode[] = [];
  let nextId = 0;

  function walk(el: Element, parentId: string | null, depth: number) {
    if (maxDepth >= 0 && depth > maxDepth)
      return;

    const nid = nextId++;
    const nodeId = String(nid);

    if (el.setAttribute)
      el.setAttribute('data-fdref', nodeId);

    let role = '';
    try { role = getAriaRole(el) || ''; } catch { /* noop */ }
    if (!role) {
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

    if (!role || role === 'none' || role === 'presentation') {
      for (const child of el.children)
        walk(child as Element, parentId, depth);
      if (el.shadowRoot) {
        for (const child of el.shadowRoot.children)
          walk(child as Element, parentId, depth);
      }
      return;
    }

    let name = '';
    try { name = getElementAccessibleName(el, false) || ''; } catch { /* noop */ }
    if (!name) {
      name = el.getAttribute?.('aria-label')
        || el.getAttribute?.('alt')
        || el.getAttribute?.('title')
        || el.getAttribute?.('placeholder')
        || '';
    }
    if (!name && el.textContent)
      name = el.textContent.trim().substring(0, 100);

    const inputEl = el as HTMLInputElement;
    const description = el.getAttribute?.('aria-description') || '';
    let checked = '';
    try {
      const c = getCheckedWithoutMixed(el);
      if (c === true)
        checked = 'true';
      else if (c === false)
        checked = 'false';
    } catch { /* noop */ }
    const disabled = !!getAriaDisabled(el);
    let readonlyVal = false;
    try { readonlyVal = getReadonly(el); } catch { /* noop */ }

    let level = 0;
    const tag = el.tagName;
    if (/^H[1-6]$/.test(tag))
      level = parseInt(tag[1]);
    const ariaLevel = el.getAttribute?.('aria-level');
    if (ariaLevel)
      level = parseInt(ariaLevel) || level;

    let valueMin = 0, valueMax = 0, valueNow = 0, valueText = '';
    if ('valueAsNumber' in el) {
      valueNow = inputEl.valueAsNumber || 0;
      valueMin = parseFloat(inputEl.min) || 0;
      valueMax = parseFloat(inputEl.max) || 100;
    }
    const ariaValueNow = el.getAttribute?.('aria-valuenow');
    if (ariaValueNow)
      valueNow = parseFloat(ariaValueNow) || 0;
    const ariaValueText = el.getAttribute?.('aria-valuetext');
    if (ariaValueText)
      valueText = ariaValueText;

    let expanded = '';
    const ariaExpanded = el.getAttribute?.('aria-expanded');
    if (ariaExpanded === 'true')
      expanded = 'true';
    else if (ariaExpanded === 'false')
      expanded = 'false';

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

    if (el.shadowRoot) {
      for (const child of el.shadowRoot.children)
        walk(child as Element, nodeId, depth + 1);
    }
    for (const child of el.children)
      walk(child as Element, nodeId, depth + 1);
  }

  walk(document.documentElement, null, 0);
  return nodes;
}

(() => {
  const fd = window.__fd;
  if (!fd || fd.__mcpSupport)
    return;
  fd.__mcpSupport = true;
  Object.assign(fd, {
    searchPage,
    findElementsCSS,
    scrollInfo,
    suggestSelectors,
    consoleErrors,
    waitForActionable,
    extractMarkdown,
    dismissDialogs,
    allElements,
    accessibilityTree,
  });
})();
