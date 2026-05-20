(function () {
  if (window.__fd_con) return;
  window.__fd_con = 1;
  var h = webkit.messageHandlers.fdConsole;
  ['log', 'warn', 'error', 'info', 'debug', 'trace'].forEach(function (l) {
    var o = console[l];
    console[l] = function () {
      try {
        h.postMessage({
          level: l,
          text: Array.prototype.map.call(arguments, function (a) {
            try { return typeof a === 'string' ? a : JSON.stringify(a); }
            catch (e) { return String(a); }
          }).join(' ')
        });
      } catch (e) {}
      return o.apply(console, arguments);
    };
  });
})();
