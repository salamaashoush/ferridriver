// Dialog auto-dismiss (alert/confirm/prompt). WKWebView's UIDelegate
// handles these natively on macOS, but stock WebKitGTK also funnels
// `script-dialog` through a signal. Both backends prefer the JS shim
// because it lets us observe `message` and `action` deterministically.
(function () {
  if (window.__fd_dlg) return;
  window.__fd_dlg = 1;
  var h = webkit.messageHandlers.fdDialog;
  window.alert = function (m) {
    try { h.postMessage({ type: 'alert', message: String(m || ''), action: 'accepted' }); } catch (e) {}
  };
  window.confirm = function (m) {
    try { h.postMessage({ type: 'confirm', message: String(m || ''), action: 'accepted' }); } catch (e) {}
    return true;
  };
  window.prompt = function (m) {
    try { h.postMessage({ type: 'prompt', message: String(m || ''), action: 'dismissed' }); } catch (e) {}
    return null;
  };
})();
