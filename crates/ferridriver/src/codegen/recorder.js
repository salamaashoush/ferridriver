// Page-side half of the recorder: turns the user's trusted input into
// actions and reports them through the `__fdRecorderBinding` exposed
// function. Runs as an init script in every document and frame, after the
// selector engine and the recorder-support highlight helpers.
//
// Every message to the binding is answered with the recorder's current
// mode, which is how a document that loads mid-recording learns whether it
// is recording, paused or asserting.
(function() {
  'use strict';
  if (window.__fdRecorder) return;
  var injected = window.__fd && window.__fd._injected;
  if (!injected) return;
  window.__fdRecorder = true;

  var genOpts = {
    testIdAttributeName: injected.testIdAttributeNameForStrictErrorAndConsoleCodegen
      ? injected.testIdAttributeNameForStrictErrorAndConsoleCodegen()
      : 'data-testid',
  };

  var mode = 'recording';
  var active = true;
  var fillBuffer = null; // { selector, locator, value, element }
  var lastUrl = location.href;
  var lastClickTime = 0;
  var highlight = null;
  var lastHoverTime = 0;
  var textCache = new Map();

  function asserting() {
    return mode === 'assertingVisibility' || mode === 'assertingText' || mode === 'assertingValue';
  }

  function send(message) {
    try {
      var reply = window.__fdRecorderBinding(JSON.stringify(message));
      if (reply && typeof reply.then === 'function') {
        reply.then(function(m) { if (typeof m === 'string') setMode(m); }, function() {});
      }
      return reply;
    } catch (e) {
      return undefined;
    }
  }

  function emit(action) {
    if (!active) return;
    send({ kind: 'action', action: action });
  }

  function describe(el, forText) {
    try {
      var options = forText ? { testIdAttributeName: genOpts.testIdAttributeName, forTextExpect: true } : genOpts;
      var selector = injected.generateSelector(el, options).selector;
      return { selector: selector, locator: injected.utils.asLocator('javascript', selector) };
    } catch (e) {
      return null;
    }
  }

  function flushFill() {
    if (!fillBuffer) return;
    var pending = fillBuffer;
    fillBuffer = null;
    emit({ type: 'fill', selector: pending.selector, locator: pending.locator, value: pending.value });
  }

  function ensureHighlight() {
    if (!highlight && injected.createHighlight && document.documentElement) {
      highlight = injected.createHighlight();
      highlight.install();
    }
    return highlight;
  }

  function clearHighlight() {
    if (highlight) highlight.clearHighlight();
  }

  function setMode(next) {
    if (next === mode) return;
    if (mode === 'recording') flushFill();
    mode = next;
    clearHighlight();
  }

  function consume(e) {
    e.preventDefault();
    e.stopPropagation();
    e.stopImmediatePropagation();
  }

  function isTextField(el) {
    var tag = (el.tagName || '').toUpperCase();
    if (tag === 'TEXTAREA' || el.isContentEditable) return true;
    if (tag !== 'INPUT') return false;
    var type = (el.type || '').toLowerCase();
    return ['checkbox', 'radio', 'file', 'button', 'submit', 'reset', 'image', 'range', 'color'].indexOf(type) === -1;
  }

  function hasValue(el) {
    var tag = (el.tagName || '').toUpperCase();
    return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT';
  }

  function assertionFor(el) {
    if (mode === 'assertingVisibility') {
      var visible = describe(el, false);
      return visible && { type: 'assertVisible', selector: visible.selector, locator: visible.locator };
    }
    if (mode === 'assertingValue') {
      if (!hasValue(el)) return null;
      var target = describe(el, false);
      if (!target) return null;
      var type = (el.type || '').toLowerCase();
      if ((el.tagName || '').toUpperCase() === 'INPUT' && (type === 'checkbox' || type === 'radio'))
        return { type: 'assertChecked', selector: target.selector, locator: target.locator, checked: !!el.checked };
      return { type: 'assertValue', selector: target.selector, locator: target.locator, value: el.value };
    }
    if (mode === 'assertingText') {
      var text = describe(el, true);
      if (!text) return null;
      textCache.clear();
      return {
        type: 'assertText',
        selector: text.selector,
        locator: text.locator,
        text: injected.utils.elementText(textCache, el).normalized,
        substring: true,
      };
    }
    return null;
  }

  // ── Hover highlight ──

  document.addEventListener('mousemove', function(e) {
    if (!e.isTrusted || !active || mode === 'none') return;
    var now = Date.now();
    if (now - lastHoverTime < 60) return;
    lastHoverTime = now;
    var el = document.elementFromPoint(e.clientX, e.clientY);
    var hl = ensureHighlight();
    if (!hl) return;
    if (!el) { hl.clearHighlight(); return; }
    var info = describe(el, mode === 'assertingText');
    if (!info) return;
    hl.updateHighlight([{ element: el, color: asserting() ? '#8acae480' : '#6fa8dc7f', tooltipText: info.locator }]);
  }, true);

  // ── Assertion picking: the click that picks never reaches the page ──

  ['pointerdown', 'mousedown', 'pointerup', 'mouseup', 'dblclick', 'auxclick', 'contextmenu'].forEach(function(type) {
    document.addEventListener(type, function(e) {
      if (e.isTrusted && active && asserting()) consume(e);
    }, true);
  });

  // ── Click ──

  document.addEventListener('click', function(e) {
    if (!e.isTrusted || !active || mode === 'none') return;
    if (asserting()) {
      consume(e);
      var picked = document.elementFromPoint(e.clientX, e.clientY) || e.target;
      var assertion = picked && assertionFor(picked);
      if (!assertion) return;
      mode = 'recording';
      clearHighlight();
      emit(assertion);
      return;
    }
    var el = e.target;
    if (!el || el.nodeType !== 1) return;
    var tag = (el.tagName || '').toUpperCase();
    var type = (el.type || '').toLowerCase();

    if (tag === 'INPUT' && (type === 'checkbox' || type === 'radio')) {
      flushFill();
      var box = describe(el, false);
      if (!box) return;
      emit({ type: el.checked ? 'check' : 'uncheck', selector: box.selector, locator: box.locator });
      return;
    }
    // A label for a checkbox clicks the box too, and that click records it.
    if (tag === 'LABEL' && el.control && el.control.tagName === 'INPUT' &&
        ['checkbox', 'radio'].indexOf((el.control.type || '').toLowerCase()) !== -1) return;
    // Text fields record what was typed; a file input records the files.
    if (isTextField(el) || (tag === 'INPUT' && type === 'file')) return;
    // A select records the option it is left on.
    if (tag === 'SELECT' || tag === 'OPTION') return;

    flushFill();
    var info = describe(el, false);
    if (!info) return;
    lastClickTime = Date.now();
    emit({ type: e.detail === 2 ? 'dblclick' : 'click', selector: info.selector, locator: info.locator });
  }, true);

  // ── Typing: one fill per field, sent when the field is left ──

  document.addEventListener('input', function(e) {
    if (!e.isTrusted || !active || mode !== 'recording') return;
    var el = e.target;
    if (!el || !isTextField(el)) return;
    var info = describe(el, false);
    if (!info) return;
    var value = el.isContentEditable ? (el.textContent || '') : el.value;
    if (fillBuffer && fillBuffer.selector !== info.selector) flushFill();
    fillBuffer = { selector: info.selector, locator: info.locator, value: value };
  }, true);

  document.addEventListener('focusout', function(e) {
    if (e.isTrusted) flushFill();
  }, true);

  // ── Change: selects and file inputs ──

  document.addEventListener('change', function(e) {
    if (!e.isTrusted || !active || mode !== 'recording') return;
    var el = e.target;
    var tag = (el.tagName || '').toUpperCase();
    if (tag === 'SELECT') {
      flushFill();
      var select = describe(el, false);
      if (!select) return;
      var values = Array.prototype.filter.call(el.options, function(o) { return o.selected; })
        .map(function(o) { return o.value; });
      emit({ type: 'select', selector: select.selector, locator: select.locator, values: values });
      return;
    }
    if (tag === 'INPUT' && (el.type || '').toLowerCase() === 'file') {
      flushFill();
      var input = describe(el, false);
      if (!input) return;
      var files = Array.prototype.map.call(el.files || [], function(f) { return f.name; });
      emit({ type: 'upload', selector: input.selector, locator: input.locator, files: files });
    }
  }, true);

  // ── Keyboard: keys that act, not keys that type ──

  var actionKeys = ['Enter', 'Tab', 'Escape'];
  var editingKeys = ['Backspace', 'Delete', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight',
    'Home', 'End', 'PageUp', 'PageDown'];
  var modifierKeys = ['Shift', 'Control', 'Alt', 'Meta'];

  document.addEventListener('keydown', function(e) {
    if (!e.isTrusted || !active) return;
    if (asserting()) {
      consume(e);
      if (e.key === 'Escape') {
        mode = 'recording';
        clearHighlight();
        send({ kind: 'mode', mode: 'recording' });
      }
      return;
    }
    if (mode !== 'recording' || modifierKeys.indexOf(e.key) !== -1) return;
    var isAction = actionKeys.indexOf(e.key) !== -1;
    var isSpecial = isAction || editingKeys.indexOf(e.key) !== -1;
    var hasMod = e.ctrlKey || e.metaKey || e.altKey;
    if (!isSpecial && !hasMod) return;
    var el = e.target;
    // In a text field the fill captures what editing keys did.
    if (el && isTextField(el) && !isAction && !hasMod) return;

    flushFill();
    var info = describe(el && el.nodeType === 1 ? el : document.body, false);
    if (!info) return;
    var key = e.key;
    if (e.shiftKey && key.length > 1) key = 'Shift+' + key;
    if (e.altKey) key = 'Alt+' + key;
    if (e.ctrlKey) key = 'Control+' + key;
    if (e.metaKey) key = 'Meta+' + key;
    emit({ type: 'press', selector: info.selector, locator: info.locator, key: key });
  }, true);

  ['keyup', 'keypress'].forEach(function(type) {
    document.addEventListener(type, function(e) {
      if (e.isTrusted && active && asserting()) consume(e);
    }, true);
  });

  // ── Same-document navigation the page did on its own ──

  function checkNav() {
    if (location.href === lastUrl) return;
    lastUrl = location.href;
    if (!active || mode !== 'recording' || window.top !== window) return;
    // A click that changed the URL already says what the user did.
    if (Date.now() - lastClickTime < 300) return;
    flushFill();
    emit({ type: 'navigate', url: lastUrl });
  }

  window.addEventListener('popstate', checkNav, true);
  window.addEventListener('hashchange', checkNav, true);
  new MutationObserver(checkNav).observe(document, { subtree: true, childList: true });

  // ── Controls the host drives ──

  window.__fdRecorderSetMode = setMode;
  // Hands back the pending fill, so a stop does not wait for the field to
  // lose focus.
  window.__fdRecorderFlush = flushFill;
  window.__fdRecorderTeardown = function() {
    flushFill();
    active = false;
    clearHighlight();
    if (highlight) highlight.uninstall();
    highlight = null;
  };

  send({ kind: 'hello' });
})();
