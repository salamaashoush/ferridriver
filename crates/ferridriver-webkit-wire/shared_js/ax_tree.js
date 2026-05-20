// JS-based accessibility tree builder. Used when the native AX tree
// is empty (e.g. data: URLs where the WebContent process doesn't
// establish the AX bridge). Walks the DOM and maps HTML/ARIA to the
// same JSON shape as the native walker so the Rust side parses both
// identically.
(function () {
  var nodes = [];
  var seq = 0;
  var RM = {
    'A': 'link', 'BUTTON': 'button', 'INPUT': 'textbox', 'TEXTAREA': 'textbox',
    'SELECT': 'combobox', 'IMG': 'img',
    'H1': 'heading', 'H2': 'heading', 'H3': 'heading',
    'H4': 'heading', 'H5': 'heading', 'H6': 'heading',
    'NAV': 'navigation', 'MAIN': 'main', 'HEADER': 'banner', 'FOOTER': 'contentinfo',
    'ASIDE': 'complementary', 'FORM': 'form',
    'TABLE': 'table', 'TR': 'row', 'TD': 'cell', 'TH': 'columnheader',
    'UL': 'list', 'OL': 'list', 'LI': 'listitem',
    'LABEL': 'label', 'PROGRESS': 'progressbar', 'DIALOG': 'dialog',
    'DETAILS': 'group', 'SECTION': 'generic', 'ARTICLE': 'article', 'SUMMARY': 'button'
  };
  var HL = { 'H1': 1, 'H2': 2, 'H3': 3, 'H4': 4, 'H5': 5, 'H6': 6 };

  nodes.push({ nodeId: 'n' + (seq++), role: 'RootWebArea', name: document.title || '', properties: [], ignored: false });

  function walk(el, pid) {
    if (!el || el.nodeType !== 1) return;
    var tag = el.tagName;
    var ar = el.getAttribute('role');
    var role = ar || RM[tag] || '';
    var nm = el.getAttribute('aria-label') || el.getAttribute('alt') || '';
    if (!nm && (tag === 'BUTTON' || tag === 'A' || tag === 'LABEL')) {
      nm = el.textContent.trim().substring(0, 200);
    }
    var isLeafText = !role && el.children.length === 0 && el.textContent.trim().length > 0;
    if (role || isLeafText) {
      var nid = 'n' + (seq++);
      var node = {
        nodeId: nid, parentId: pid,
        role: role || (isLeafText ? 'StaticText' : 'generic'),
        properties: [], ignored: false
      };
      if (nm) node.name = nm;
      if (isLeafText) node.name = el.textContent.trim().substring(0, 500);
      var hl = HL[tag];
      if (hl) node.properties.push({ name: 'level', value: hl });
      if (tag === 'INPUT' || tag === 'TEXTAREA') {
        var t = el.type || 'text';
        if (t === 'checkbox') node.role = 'checkbox';
        else if (t === 'radio') node.role = 'radio';
        else if (t === 'submit' || t === 'button') node.role = 'button';
        if (el.value) node.properties.push({ name: 'value', value: el.value });
        if (el.disabled) node.properties.push({ name: 'disabled', value: true });
        if (el.required) node.properties.push({ name: 'required', value: true });
      }
      if (el.getAttribute('aria-checked')) node.properties.push({ name: 'checked', value: el.getAttribute('aria-checked') === 'true' });
      if (el.getAttribute('aria-expanded')) node.properties.push({ name: 'expanded', value: el.getAttribute('aria-expanded') === 'true' });
      if (el.getAttribute('aria-selected')) node.properties.push({ name: 'selected', value: el.getAttribute('aria-selected') === 'true' });
      nodes.push(node);
      for (var i = 0; i < el.children.length; i++) walk(el.children[i], nid);
    } else {
      for (var i = 0; i < el.children.length; i++) walk(el.children[i], pid);
    }
  }

  if (document.body) walk(document.body, 'n0');
  return JSON.stringify(nodes);
})()
