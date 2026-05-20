// Network observation + route interception (fetch + XMLHttpRequest).
// Routes are RegExp patterns stored in window.__fd_routes. When a
// fetch/XHR URL matches, we call webkit.messageHandlers.fdRoute.postMessage
// which round-trips to the host runtime for the route action
// (fulfill/continue/abort).
(function () {
  if (window.__fd_net) return;
  window.__fd_net = 1;
  var hNet = webkit.messageHandlers.fdNetwork;
  var seq = 0;
  window.__fd_routes = window.__fd_routes || [];
  function matchRoute(url) {
    for (var i = 0; i < window.__fd_routes.length; i++) {
      if (window.__fd_routes[i].test(url)) return true;
    }
    return false;
  }

  // fetch interceptor
  var origFetch = window.fetch;
  window.fetch = function (input, opts) {
    var method = (opts && opts.method) || 'GET';
    var u = typeof input === 'string' ? input : (input && input.url || '');
    var rid = 'f' + (seq++);
    try { hNet.postMessage({ id: rid, method: method, url: u, resourceType: 'Fetch' }); } catch (e) {}

    if (!matchRoute(u)) {
      // No route — let fetch run normally, observe outcome.
      return origFetch.apply(this, arguments).then(function (resp) {
        var headers = {};
        try { resp.headers.forEach(function (v, k) { headers[k] = v; }); } catch (e) {}
        try { hNet.postMessage({ kind: 'response', id: rid, status: resp.status, statusText: resp.statusText, url: resp.url, headers: headers }); } catch (e) {}
        return resp;
      }).catch(function (err) {
        try { hNet.postMessage({ kind: 'failure', id: rid, errorText: String((err && err.message) || err) }); } catch (e) {}
        throw err;
      });
    }

    // Route matches — ask host runtime for the action.
    var hdrs = '{}';
    try {
      if (opts && opts.headers) {
        hdrs = JSON.stringify(Object.fromEntries(
          opts.headers instanceof Headers ? opts.headers.entries() : Object.entries(opts.headers)
        ));
      }
    } catch (e) {}
    var body = (opts && opts.body) || '';

    return webkit.messageHandlers.fdRoute.postMessage({
      url: u, method: method, headers: hdrs, postData: typeof body === 'string' ? body : ''
    }).then(function (action) {
      if (!action || action.action === 'continue') {
        return origFetch.apply(null, [input, opts]).then(function (resp) {
          var hh = {};
          try { resp.headers.forEach(function (v, k) { hh[k] = v; }); } catch (e) {}
          try { hNet.postMessage({ kind: 'response', id: rid, status: resp.status, statusText: resp.statusText, url: resp.url, headers: hh }); } catch (e) {}
          return resp;
        }).catch(function (err) {
          try { hNet.postMessage({ kind: 'failure', id: rid, errorText: String((err && err.message) || err) }); } catch (e) {}
          throw err;
        });
      }
      if (action.action === 'abort') {
        try { hNet.postMessage({ kind: 'failure', id: rid, errorText: 'blockedbyclient' }); } catch (e) {}
        throw new TypeError('Request blocked by route');
      }
      if (action.action === 'fulfill') {
        var h = new Headers();
        if (action.headers) { for (var k in action.headers) h.set(k, action.headers[k]); }
        if (action.contentType) h.set('content-type', action.contentType);
        var hh2 = {};
        h.forEach(function (v, k) { hh2[k] = v; });
        try { hNet.postMessage({ kind: 'response', id: rid, status: action.status || 200, statusText: 'OK', url: u, headers: hh2 }); } catch (e) {}
        return new Response(action.body || '', { status: action.status || 200, headers: h });
      }
      return origFetch.apply(null, [input, opts]);
    });
  };

  // XHR interceptor
  var origOpen = XMLHttpRequest.prototype.open;
  var origSend = XMLHttpRequest.prototype.send;
  XMLHttpRequest.prototype.open = function (method, url) {
    this.__fd_method = method;
    this.__fd_url = url;
    try { hNet.postMessage({ id: 'x' + (seq++), method: method, url: url, resourceType: 'XHR' }); } catch (e) {}
    return origOpen.apply(this, arguments);
  };
  XMLHttpRequest.prototype.send = function (body) {
    var self = this;
    var url = this.__fd_url || '';
    var method = this.__fd_method || 'GET';
    if (!matchRoute(url)) return origSend.apply(this, arguments);
    webkit.messageHandlers.fdRoute.postMessage({
      url: url, method: method, headers: '{}', postData: typeof body === 'string' ? body : ''
    }).then(function (action) {
      if (!action || action.action === 'continue') return origSend.apply(self, [body]);
      if (action.action === 'abort') {
        Object.defineProperty(self, 'status', { get: function () { return 0; } });
        Object.defineProperty(self, 'readyState', { get: function () { return 4; } });
        self.dispatchEvent(new Event('error'));
        return;
      }
      if (action.action === 'fulfill') {
        Object.defineProperty(self, 'status', { get: function () { return action.status || 200; } });
        Object.defineProperty(self, 'responseText', { get: function () { return action.body || ''; } });
        Object.defineProperty(self, 'response', { get: function () { return action.body || ''; } });
        Object.defineProperty(self, 'readyState', { get: function () { return 4; } });
        self.dispatchEvent(new Event('readystatechange'));
        self.dispatchEvent(new Event('load'));
        return;
      }
      origSend.apply(self, [body]);
    });
  };
})();
