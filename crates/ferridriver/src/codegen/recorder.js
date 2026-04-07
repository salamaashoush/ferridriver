// Interactive recorder: captures user interactions and sends them to Rust.
// Injected via add_init_script — survives page navigations.
// Depends on window.__fd (engine.min.js) being present.
(function() {
  'use strict';
  if (window.__fdRecorder) return;
  window.__fdRecorder = true;

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

  // ── State ──

  var fillBuffer = null; // { selector, locator, value, timer }
  var lastUrl = location.href;
  var lastClickTime = 0; // suppress navigation emitted right after a click

  function emit(action) {
    try { __fdRecorderAction(JSON.stringify(action)); } catch (e) { /* binding not ready */ }
  }

  function flushFill() {
    if (!fillBuffer) return;
    clearTimeout(fillBuffer.timer);
    emit({ type: 'fill', selector: fillBuffer.selector, locator: fillBuffer.locator, value: fillBuffer.value });
    fillBuffer = null;
  }

  // ── Highlight on hover ──

  var highlight = injected.createHighlight();
  highlight.install();
  var lastHoverTime = 0;

  document.addEventListener('mousemove', function(e) {
    // Throttle to ~60ms to avoid jank.
    var now = Date.now();
    if (now - lastHoverTime < 60) return;
    lastHoverTime = now;

    var el = document.elementFromPoint(e.clientX, e.clientY);
    if (!el) { highlight.clearHighlight(); return; }

    var info = bestSelector(el);
    if (!info) return;
    highlight.updateHighlight([{ element: el, color: '#6fa8dc7f', tooltipText: info.locator }]);
  }, true);

  // ── Click ──

  document.addEventListener('click', function(e) {
    if (!e.isTrusted) return;
    var el = e.target;
    if (!el) return;

    var tag = (el.tagName || '').toUpperCase();
    var type = (el.type || '').toLowerCase();

    // Checkbox / radio -> check/uncheck
    if (tag === 'INPUT' && (type === 'checkbox' || type === 'radio')) {
      flushFill();
      var info = bestSelector(el);
      if (!info) return;
      emit({ type: el.checked ? 'check' : 'uncheck', selector: info.selector, locator: info.locator });
      return;
    }

    // Skip click on text inputs (fill handles them)
    if (tag === 'INPUT' || tag === 'TEXTAREA' || el.isContentEditable) return;
    // Skip click on select (change handler handles it)
    if (tag === 'SELECT' || tag === 'OPTION') return;

    flushFill();
    var info = bestSelector(el);
    if (!info) return;
    lastClickTime = Date.now();
    emit({ type: 'click', selector: info.selector, locator: info.locator });
  }, true);

  // ── Double-click ──

  document.addEventListener('dblclick', function(e) {
    if (!e.isTrusted) return;
    var el = e.target;
    if (!el) return;
    var info = bestSelector(el);
    if (!info) return;
    emit({ type: 'dblclick', selector: info.selector, locator: info.locator });
  }, true);

  // ── Input (fill coalescing) ──

  document.addEventListener('input', function(e) {
    if (!e.isTrusted) return;
    var el = e.target;
    if (!(el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement || el.isContentEditable)) return;
    // Skip checkbox/radio — handled by click -> check/uncheck.
    var inputType = (el.type || '').toLowerCase();
    if (inputType === 'checkbox' || inputType === 'radio') return;

    var info = bestSelector(el);
    if (!info) return;
    var value = el.value !== undefined ? el.value : (el.textContent || '');

    if (fillBuffer && fillBuffer.selector === info.selector) {
      clearTimeout(fillBuffer.timer);
      fillBuffer.value = value;
    } else {
      flushFill();
      fillBuffer = { selector: info.selector, locator: info.locator, value: value };
    }
    fillBuffer.timer = setTimeout(flushFill, 800);
  }, true);

  // ── Change on <select> ──

  document.addEventListener('change', function(e) {
    if (!e.isTrusted) return;
    var el = e.target;
    if ((el.tagName || '').toUpperCase() !== 'SELECT') return;

    flushFill();
    var info = bestSelector(el);
    if (!info) return;
    emit({ type: 'select', selector: info.selector, locator: info.locator, value: el.value });
  }, true);

  // ── Keyboard (special keys only) ──

  // Keys that should emit press events.
  var actionKeys = ['Enter', 'Tab', 'Escape'];
  // Keys that are editing operations — suppressed in text inputs (fill captures the result).
  var editingKeys = ['Backspace', 'Delete', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight',
    'Home', 'End', 'PageUp', 'PageDown'];
  var allSpecialKeys = actionKeys.concat(editingKeys);

  document.addEventListener('keydown', function(e) {
    if (!e.isTrusted) return;

    var isSpecial = allSpecialKeys.indexOf(e.key) !== -1;
    var isAction = actionKeys.indexOf(e.key) !== -1;
    var hasMod = e.ctrlKey || e.metaKey || e.altKey;

    // Only record special keys and modifier combos (not regular typing — fill handles it)
    if (!isSpecial && !hasMod) return;

    // In text inputs: only emit action keys (Enter/Tab/Escape) and modifier combos.
    // Editing keys (Backspace, arrows, etc.) are suppressed — fill coalescing captures the result.
    var el = e.target;
    var tag = (el.tagName || '').toUpperCase();
    var isTextInput = tag === 'INPUT' || tag === 'TEXTAREA' || el.isContentEditable;
    if (isTextInput && !isAction && !hasMod) return;

    flushFill();
    var info = bestSelector(el);
    if (!info) return;

    var key = e.key;
    if (e.metaKey && key !== 'Meta') key = 'Meta+' + key;
    if (e.ctrlKey && key !== 'Control') key = 'Control+' + key;
    if (e.altKey && key !== 'Alt') key = 'Alt+' + key;
    if (e.shiftKey && key.length > 1 && key !== 'Shift') key = 'Shift+' + key;

    emit({ type: 'press', selector: info.selector, locator: info.locator, key: key });
  }, true);

  // ── Navigation detection ──

  function checkNav() {
    if (location.href !== lastUrl) {
      lastUrl = location.href;
      // Suppress navigation right after a click (the click already captures the intent).
      if (Date.now() - lastClickTime < 300) return;
      flushFill();
      emit({ type: 'navigate', url: lastUrl });
    }
  }

  window.addEventListener('popstate', checkNav, true);
  window.addEventListener('hashchange', checkNav, true);
  // SPA navigation detection via title/URL mutation
  new MutationObserver(checkNav).observe(document, { subtree: true, childList: true });
})();
