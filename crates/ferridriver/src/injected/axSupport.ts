import { getAriaDisabled, getAriaRole, getCheckedWithoutMixed, getElementAccessibleName, getReadonly } from './roleUtils';

declare global {
  interface Window { __fd: any; }
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
  if (!fd || fd.__axSupport)
    return;
  fd.__axSupport = true;
  Object.assign(fd, { accessibilityTree });
})();
