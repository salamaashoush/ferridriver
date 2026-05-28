// Interactive locator picker: highlights elements on hover and records the
// generated selector for the element the user clicks. Mirrors the behavior
// behind Playwright's `page.pickLocator()` (recorder "inspecting" mode).
// The selector is stored on `window.__fdPickedSelector`; the Rust side
// polls for it rather than relying on a cross-task exposed-function call,
// which keeps engine teardown race-free.
// Depends on window.__fd (engine.min.js) and the recorder-support highlight
// helpers being present.
(function() {
  'use strict';
  if (window.__fdPicker) return;
  window.__fdPicker = true;
  window.__fdPickedSelector = undefined;

  var injected = window.__fd && window.__fd._injected;
  if (!injected) return;

  var genOpts = { testIdAttributeName: 'data-testid' };

  function bestSelector(el) {
    try {
      var result = injected.generateSelector(el, genOpts);
      var selector = result.selector;
      var locator = injected.utils.asLocator('javascript', selector);
      return { selector: selector, locator: locator };
    } catch (e) {
      return null;
    }
  }

  var highlight = injected.createHighlight();
  highlight.install();
  var lastHoverTime = 0;

  function teardown() {
    if (!window.__fdPicker) return;
    window.__fdPicker = false;
    window.__fdPickerReady = false;
    try { highlight.clearHighlight(); } catch (e) { /* ignore */ }
    try { highlight.uninstall(); } catch (e) { /* ignore */ }
    document.removeEventListener('mousemove', onMove, true);
    document.removeEventListener('click', onClick, true);
    document.removeEventListener('mousedown', swallow, true);
    document.removeEventListener('mouseup', swallow, true);
  }

  // Exposed so cancelPickLocator()/hideHighlight() can stop the picker.
  window.__fdPickerCancel = teardown;

  function swallow(e) {
    if (!e.isTrusted) return;
    e.preventDefault();
    e.stopPropagation();
    e.stopImmediatePropagation();
  }

  function onMove(e) {
    var now = Date.now();
    if (now - lastHoverTime < 30) return;
    lastHoverTime = now;
    var el = document.elementFromPoint(e.clientX, e.clientY);
    if (!el) { highlight.clearHighlight(); return; }
    var info = bestSelector(el);
    if (!info) return;
    highlight.updateHighlight([{ element: el, color: '#6fa8dc7f', tooltipText: info.locator }]);
  }

  function onClick(e) {
    if (!e.isTrusted) return;
    e.preventDefault();
    e.stopPropagation();
    e.stopImmediatePropagation();
    var el = document.elementFromPoint(e.clientX, e.clientY) || e.target;
    if (!el) return;
    var info = bestSelector(el);
    if (!info) return;
    window.__fdPickedSelector = info.selector;
    teardown();
  }

  document.addEventListener('mousemove', onMove, true);
  document.addEventListener('click', onClick, true);
  document.addEventListener('mousedown', swallow, true);
  document.addEventListener('mouseup', swallow, true);
  window.__fdPickerReady = true;
})();
