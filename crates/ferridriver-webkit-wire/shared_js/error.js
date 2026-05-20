// Uncaught JS error + unhandled rejection capture. Mirrors
// `Runtime.exceptionThrown` (CDP) / `log.entryAdded type=javascript` (BiDi)
// — routes a synthesised {name, message, stack} through the existing
// fdConsole IPC frame with a distinguishing `level: 'pageerror'` marker.
// The Rust-side drainer routes `pageerror` to PageEvent::PageError(WebError)
// instead of PageEvent::Console(ConsoleMessage).
(function () {
  if (window.__fd_err) return;
  window.__fd_err = 1;
  var h = webkit.messageHandlers.fdConsole;
  window.addEventListener('error', function (e) {
    try {
      var err = e.error || {};
      var name = err.name || 'Error';
      var msg = err.message || (e.message || String(err));
      var stack = err.stack || '';
      h.postMessage({ level: 'pageerror', text: name + ': ' + msg + (stack ? '\n' + stack : '') });
    } catch (x) {}
  });
  window.addEventListener('unhandledrejection', function (e) {
    try {
      var reason = e.reason;
      var name = 'Error';
      var msg = String(reason);
      var stack = '';
      if (reason && typeof reason === 'object') {
        name = reason.name || 'Error';
        msg = reason.message || String(reason);
        stack = reason.stack || '';
      }
      h.postMessage({ level: 'pageerror', text: name + ': ' + msg + (stack ? '\n' + stack : '') });
    } catch (x) {}
  });
})();
