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

(() => {
  const fd = window.__fd;
  if (!fd || fd.__mcpSupport)
    return;
  fd.__mcpSupport = true;
  Object.assign(fd, {
    searchPage,
    findElementsCSS,
    suggestSelectors,
    consoleErrors,
    extractMarkdown,
    allElements,
  });
})();
