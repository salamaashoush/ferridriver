// Body passed to WKWebView.callAsyncJavaScript / WebView.call_async_javascript_function.
// `__fd_expr` is supplied as a named argument; the wrapper awaits, then JSON-stringifies
// the result so the wire format stays primitive (string or null).
var __fd_r = await eval(__fd_expr);
if (__fd_r === undefined || __fd_r === null) return null;
try { return JSON.stringify(__fd_r); }
catch (e) { return String(__fd_r); }
