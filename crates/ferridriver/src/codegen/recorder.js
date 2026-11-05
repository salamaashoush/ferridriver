// Page-side half of the recorder: turns the user's input into actions and
// reports them through the `__fdRecorderBinding` exposed binding. Runs as
// an init script in every document and frame of the recorded context,
// after the selector engine and the recorder-support highlight helpers.
//
// The binding knows which page and frame called it, so a frame reports its
// own selectors and the host puts the frame path in front of them.
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

  var testIdAttributeName = injected.testIdAttributeNameForStrictErrorAndConsoleCodegen
    ? injected.testIdAttributeNameForStrictErrorAndConsoleCodegen()
    : 'data-testid';

  var mode = 'recording';
  var active = true;
  var lastUrl = location.href;
  var lastClickTime = 0;
  var highlight = null;
  var textCache = new Map();
  // What a click records: the element under the pointer when it last
  // moved, not the event target, so a click that lands while the page is
  // re-rendering still names what the person was pointing at.
  var hoveredElement = null;
  var hoveredModel = null;
  // The focused element's selector, taken when it gained focus, so typing
  // into a field whose text changes the selector keeps naming the field.
  var activeModel = null;
  var observer = null;

  function asserting() {
    return mode === 'assertingVisibility' || mode === 'assertingText' || mode === 'assertingValue';
  }

  function recording() {
    return active && mode === 'recording';
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

  function generate(el, options) {
    var generated = injected.generateSelector(el, Object.assign({ testIdAttributeName: testIdAttributeName }, options || {}));
    return generated && generated.selector ? generated : null;
  }

  function describeSelector(selector) {
    return { selector: selector, locator: injected.utils.asLocator('javascript', selector) };
  }

  function describe(el, options) {
    try {
      var generated = generate(el, options);
      return generated && describeSelector(generated.selector);
    } catch (e) {
      return null;
    }
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

  function showHighlight(elements, selector) {
    var hl = ensureHighlight();
    if (!hl) return;
    if (!elements || !elements.length) { hl.clearHighlight(); return; }
    var tooltipText = injected.utils.asLocator('javascript', selector);
    var color = asserting() ? '#8acae480' : '#6fa8dc7f';
    hl.updateHighlight(elements.map(function(element) {
      return { element: element, color: color, tooltipText: tooltipText };
    }));
  }

  function setMode(next) {
    if (next === mode) return;
    mode = next;
    resetHovered();
    activeModel = null;
  }

  function consume(e) {
    e.preventDefault();
    e.stopPropagation();
    e.stopImmediatePropagation();
  }

  // The highlight's own glass pane is not the page.
  function onOverlay(e) {
    return e.composedPath().some(function(node) {
      return ((node && node.nodeName) || '').toLowerCase() === 'x-pw-glass';
    });
  }

  function deepEventTarget(e) {
    return e.composedPath()[0];
  }

  function deepActiveElement() {
    var element = document.activeElement;
    while (element && element.shadowRoot && element.shadowRoot.activeElement)
      element = element.shadowRoot.activeElement;
    return element;
  }

  function asCheckbox(node) {
    if (!node || node.nodeName !== 'INPUT') return null;
    return ['checkbox', 'radio'].indexOf(node.type) !== -1 ? node : null;
  }

  function isRangeInput(node) {
    return !!node && node.nodeName === 'INPUT' && (node.type || '').toLowerCase() === 'range';
  }

  // Controls that open a native picker: the pick, not the click, is what
  // the person did.
  var nativePickerInputTypes = ['color', 'date', 'datetime-local', 'file', 'month', 'range', 'time', 'week'];

  function ignoredMouseTarget(node) {
    if (!node) return true;
    if (node.nodeName === 'SELECT' || node.nodeName === 'OPTION') return true;
    return node.nodeName === 'INPUT' && nativePickerInputTypes.indexOf(node.type) !== -1;
  }

  function selectorFor(element, options) {
    var generated = generate(element, options);
    return generated ? generated.selector : null;
  }

  function hoveredSelectorOr(element) {
    return hoveredModel ? hoveredModel.selector : selectorFor(element);
  }

  function activeSelectorFor(element) {
    if (activeModel && activeModel.elements[0] === element) return activeModel.selector;
    return selectorFor(element);
  }

  function record(action, selector) {
    if (!selector) return;
    var described = describeSelector(selector);
    action.selector = described.selector;
    action.locator = described.locator;
    emit(action);
  }

  function hasValue(el) {
    var tag = (el.tagName || '').toUpperCase();
    return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT';
  }

  function assertionFor(el) {
    if (mode === 'assertingVisibility') {
      var visible = describe(el);
      return visible && { type: 'assertVisible', selector: visible.selector, locator: visible.locator };
    }
    if (mode === 'assertingValue') {
      if (!hasValue(el)) return null;
      var target = describe(el);
      if (!target) return null;
      var type = (el.type || '').toLowerCase();
      if ((el.tagName || '').toUpperCase() === 'INPUT' && (type === 'checkbox' || type === 'radio'))
        return { type: 'assertChecked', selector: target.selector, locator: target.locator, checked: !!el.checked };
      return { type: 'assertValue', selector: target.selector, locator: target.locator, value: el.value };
    }
    if (mode === 'assertingText') {
      var text = describe(el, { forTextExpect: true });
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

  // ── What the pointer is over ──

  function installObserver() {
    if (observer || !document.body) return;
    observer = new MutationObserver(function(mutations) {
      if (!hoveredElement) return;
      for (var i = 0; i < mutations.length; i++) {
        var removed = mutations[i].removedNodes;
        for (var j = 0; j < removed.length; j++) {
          if (removed[j] === hoveredElement || removed[j].contains(hoveredElement)) resetHovered();
        }
      }
    });
    observer.observe(document.body, { childList: true, subtree: true });
  }

  function resetHovered() {
    hoveredElement = null;
    hoveredModel = null;
    clearHighlight();
  }

  function updateHovered() {
    installObserver();
    if (!hoveredElement || !hoveredElement.isConnected) {
      resetHovered();
      return;
    }
    var generated;
    try {
      generated = generate(hoveredElement, mode === 'assertingText' ? { forTextExpect: true } : undefined);
    } catch (e) {
      generated = null;
    }
    if (hoveredModel && generated && hoveredModel.selector === generated.selector) return;
    hoveredModel = generated ? { selector: generated.selector, elements: generated.elements } : null;
    if (hoveredModel) showHighlight(hoveredModel.elements, hoveredModel.selector);
    else clearHighlight();
  }

  document.addEventListener('mousemove', function(e) {
    if (!e.isTrusted || !active || mode === 'none' || onOverlay(e)) return;
    var target = deepEventTarget(e);
    if (hoveredElement === target) return;
    hoveredElement = target;
    updateHovered();
  }, true);

  document.addEventListener('mouseleave', function(e) {
    if (!e.isTrusted || !active || onOverlay(e)) return;
    // Leaving an iframe.
    if (window.top !== window && deepEventTarget(e).nodeType === Node.DOCUMENT_NODE) {
      hoveredElement = null;
      updateHovered();
    }
  }, true);

  document.addEventListener('scroll', function(e) {
    if (!e.isTrusted || !active) return;
    resetHovered();
  }, true);

  document.addEventListener('focus', function(e) {
    if (!e.isTrusted || !active || mode !== 'recording' || onOverlay(e)) return;
    var element = deepActiveElement();
    // Firefox focuses the body when a backgrounded window is clicked.
    if (element === document.body) return;
    var generated = null;
    try {
      generated = element ? generate(element) : null;
    } catch (err) {
      generated = null;
    }
    activeModel = generated ? { selector: generated.selector, elements: generated.elements } : null;
    hoveredElement = element;
    updateHovered();
  }, true);

  document.addEventListener('mousedown', function(e) {
    if (!e.isTrusted || !active || onOverlay(e)) return;
    if (asserting()) { consume(e); return; }
    if (mode !== 'recording' || ignoredMouseTarget(deepEventTarget(e))) return;
    activeModel = hoveredModel;
  }, true);

  // ── Assertion picking: the click that picks never reaches the page ──

  ['pointerdown', 'pointerup', 'mouseup', 'dblclick', 'auxclick', 'contextmenu'].forEach(function(type) {
    document.addEventListener(type, function(e) {
      if (e.isTrusted && active && asserting()) consume(e);
    }, true);
  });

  // ── Click ──

  document.addEventListener('click', function(e) {
    if (!e.isTrusted || !active || mode === 'none' || onOverlay(e)) return;
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
    // Sliding a range can end in a click on another element.
    if (isRangeInput(hoveredElement)) return;
    var target = deepEventTarget(e);
    if (ignoredMouseTarget(target)) return;
    // A click the keyboard made (Enter on a button, Space on a checkbox)
    // has no click count and is recorded as the key press.
    if (e.detail === 0) return;

    var checkbox = asCheckbox(target);
    if (checkbox && e.detail === 1) {
      // `checked` already holds the state the click left.
      record({ type: checkbox.checked ? 'check' : 'uncheck' }, hoveredSelectorOr(target));
      return;
    }
    recordClick(e, buttonFor(e), e.detail, target);
  }, true);

  // A middle click; a right click is the context menu's.
  document.addEventListener('auxclick', function(e) {
    if (!e.isTrusted || !active || mode !== 'recording' || onOverlay(e) || e.button === 2) return;
    var target = deepEventTarget(e);
    if (ignoredMouseTarget(target)) return;
    recordClick(e, buttonFor(e), e.detail || 1, target);
  }, true);

  // A context menu the keyboard opened is not a click.
  document.addEventListener('contextmenu', function(e) {
    if (!e.isTrusted || !active || mode !== 'recording' || onOverlay(e) || e.button !== 2) return;
    var target = deepEventTarget(e);
    if (ignoredMouseTarget(target)) return;
    recordClick(e, 'right', 1, target);
  }, true);

  function buttonFor(e) {
    if (e.which === 2) return 'middle';
    if (e.which === 3) return 'right';
    return 'left';
  }

  function modifiersFor(e) {
    var modifiers = [];
    if (e.altKey) modifiers.push('Alt');
    if (e.ctrlKey) modifiers.push('ControlOrMeta');
    if (e.metaKey) modifiers.push('ControlOrMeta');
    if (e.shiftKey) modifiers.push('Shift');
    return modifiers;
  }

  // The options a click is replayed with: only the ones that differ from a
  // plain left click, a double click's count being its name, and a
  // position only on a canvas, where the point is the target.
  function clickOptions(e, button, clickCount) {
    var options = {};
    if (button !== 'left') options.button = button;
    var modifiers = modifiersFor(e);
    if (modifiers.length) options.modifiers = modifiers;
    if (clickCount > 2) options.clickCount = clickCount;
    if (e.target && e.target.nodeName === 'CANVAS')
      options.position = { x: Math.round(e.offsetX), y: Math.round(e.offsetY) };
    return options;
  }

  function recordClick(e, button, clickCount, target) {
    lastClickTime = Date.now();
    var action = { type: clickCount === 2 ? 'dblclick' : 'click' };
    var options = clickOptions(e, button, clickCount);
    if (Object.keys(options).length) action.options = options;
    record(action, hoveredSelectorOr(target));
  }

  // ── Input: every change of value, trusted or not ──
  //
  // A select set from script reports its change as an untrusted `input`,
  // and that is still what the person chose.

  document.addEventListener('input', function(e) {
    if (!recording() || onOverlay(e)) return;
    var target = deepEventTarget(e);
    if (!target || target.nodeType !== 1) return;

    if (target.nodeName === 'INPUT' && target.type.toLowerCase() === 'file') {
      // A hidden file input opened by another element is not what the
      // pointer was over.
      var fileSelector = target === hoveredElement && hoveredModel ? hoveredModel.selector : selectorFor(target);
      var files = Array.prototype.map.call(target.files || [], function(f) { return f.name; });
      record({ type: 'upload', files: files }, fileSelector);
      return;
    }

    if (isRangeInput(target)) {
      record({ type: 'fill', value: target.value }, hoveredSelectorOr(target));
      return;
    }

    if (target.nodeName === 'INPUT' || target.nodeName === 'TEXTAREA' || target.isContentEditable) {
      if (asCheckbox(target)) return;
      // A contenteditable already holds the new text, so its selector must
      // not depend on that text.
      var selector = target.isContentEditable ? selectorFor(target, { noText: true }) : activeSelectorFor(target);
      record({ type: 'fill', value: target.isContentEditable ? target.innerText : target.value }, selector);
      return;
    }

    if (target.nodeName === 'SELECT') {
      var values = Array.prototype.map.call(target.selectedOptions, function(o) { return o.value; });
      record({ type: 'select', values: values }, activeSelectorFor(target));
    }
  }, true);

  // ── Keyboard: keys that act, not keys that type ──

  function shouldRecordKey(e) {
    var key = e.key;
    // An IME can send a key event with no key (autofill).
    if (typeof key !== 'string') return false;
    var target = deepEventTarget(e);
    // A new line in a multi-line field is part of what the fill records,
    // and so is what Backspace, Delete and AltGraph change.
    if (key === 'Enter' && (target.nodeName === 'TEXTAREA' || target.isContentEditable)) return false;
    if (['Backspace', 'Delete', 'AltGraph'].indexOf(key) !== -1) return false;
    // The QWERTZ at sign on macOS.
    if (key === '@' && e.code === 'KeyL') return false;
    // Pasting changes the value, which the fill records.
    if (navigator.platform.indexOf('Mac') !== -1) {
      if (key === 'v' && e.metaKey) return false;
    } else {
      if (key === 'v' && e.ctrlKey) return false;
      if (key === 'Insert' && e.shiftKey) return false;
    }
    if (['Shift', 'Control', 'Meta', 'Alt', 'Process'].indexOf(key) !== -1) return false;
    var hasModifier = e.ctrlKey || e.altKey || e.metaKey;
    if (key.length === 1 && !hasModifier) return !!asCheckbox(target);
    return true;
  }

  function shortcut(e) {
    var parts = [];
    if (e.altKey) parts.push('Alt');
    if (e.ctrlKey) parts.push('ControlOrMeta');
    if (e.metaKey) parts.push('ControlOrMeta');
    if (e.shiftKey) parts.push('Shift');
    parts.push(e.key);
    return parts.join('+');
  }

  document.addEventListener('keydown', function(e) {
    if (!e.isTrusted || !active || onOverlay(e)) return;
    if (asserting()) {
      consume(e);
      if (e.key === 'Escape') {
        mode = 'recording';
        clearHighlight();
        send({ kind: 'mode', mode: 'recording' });
      }
      return;
    }
    if (mode !== 'recording' || !shouldRecordKey(e)) return;
    var target = deepEventTarget(e);
    if (e.key === ' ') {
      var checkbox = asCheckbox(target);
      // The box toggles after keydown, so it is about to hold the opposite.
      if (checkbox && e.detail === 0) {
        record({ type: checkbox.checked ? 'uncheck' : 'check' }, activeSelectorFor(target));
        return;
      }
    }
    record({ type: 'press', key: shortcut(e) }, activeSelectorFor(target));
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
    if (!recording() || window.top !== window) return;
    // A click that changed the URL already says what the user did.
    if (Date.now() - lastClickTime < 300) return;
    emit({ type: 'navigate', url: lastUrl });
  }

  window.addEventListener('popstate', checkNav, true);
  window.addEventListener('hashchange', checkNav, true);
  new MutationObserver(checkNav).observe(document, { subtree: true, childList: true });

  // ── Controls the host drives ──

  window.__fdRecorderSetMode = setMode;
  window.__fdRecorderTeardown = function() {
    active = false;
    resetHovered();
    activeModel = null;
    if (observer) observer.disconnect();
    observer = null;
    if (highlight) highlight.uninstall();
    highlight = null;
  };

  send({ kind: 'hello' });
})();
