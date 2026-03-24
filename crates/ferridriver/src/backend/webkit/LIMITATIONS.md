# WebKit Backend Limitations

ferridriver's WebKit backend uses standard WKWebView (Apple's public framework).
Playwright uses a **custom patched WebKit fork** with 356 modified files adding
a proprietary inspector protocol. These are the features that require that fork
and cannot be implemented on standard WKWebView.

## Cannot implement (require WebKit source patches)

### Network
- **Request interception/mocking** - Cannot intercept, modify, or mock HTTP
  requests at the network layer. Our fetch/XHR JS interception covers
  JS-initiated requests but misses navigation, iframes, images, CSS, fonts.
- **Disable resource caching** - No API to control WebKit's network cache.
- **Real offline mode** - Our `navigator.onLine` override is cosmetic. Actual
  network requests still go through. Playwright's fork blocks at the network
  layer.
- **Response body access** - Cannot read response bodies of network requests.

### Security
- **Bypass CSP** - Content-Security-Policy enforcement is deep in WebKit's
  loader. Pages with strict CSP may block our injected scripts.
- **Ignore certificate errors** - No API to accept self-signed or invalid
  certificates.

### Page Control
- **Bootstrap script before document creation** - Our WKUserScript runs at
  "document start" which is after the document object exists. Playwright's
  `Page.setBootstrapScript` runs before document creation, guaranteeing no
  page JS can execute first.
- **Isolated JS worlds** - Cannot create separate JS execution contexts.
  Our automation code runs in the same world as page scripts and can be
  detected or interfered with.
- **Per-page feature toggles** - Cannot toggle DeviceOrientation, FullScreen,
  Notifications, PointerLock, or other browser features per-page.

### Browser
- **Permissions** - No API to grant/deny geolocation, camera, microphone,
  notifications, or other permissions programmatically.
- **Browser contexts** - No API to create isolated browser contexts with
  separate cookie jars, caches, and permissions.
- **Auth credentials** - No API to provide HTTP Basic Auth credentials.
- **Download control** - No API to control download behavior or cancel
  downloads.
- **Screencast** - No API for video recording of the page.

## Implemented via JS workarounds (functional but not engine-level)

These work for most use cases but differ from Playwright's native implementation:

| Feature | Our approach | Limitation |
|---------|-------------|------------|
| Timezone | WKUserScript overriding Intl/Date | Only affects JS APIs, not HTTP headers |
| Locale | WKUserScript overriding navigator.language + Intl | Only affects JS APIs |
| Forced colors | matchMedia interception | CSS `@media (forced-colors)` not affected |
| Contrast | matchMedia interception | CSS `@media (prefers-contrast)` not affected |
| Reduced motion | matchMedia interception | CSS `@media (prefers-reduced-motion)` not affected |
| Focus | document.hasFocus override | OS-level focus unchanged |
| Extra HTTP headers | fetch/XHR interception | Navigation/resource requests not covered |
| Offline | navigator.onLine override | Network still works |

## Implemented natively (full parity with Playwright)

| Feature | API used |
|---------|---------|
| Color scheme (dark/light) | NSAppearance override |
| Media type (screen/print) | WKWebView.setMediaType: |
| Viewport/device metrics | WKWebView frame + window resize |
| User agent | OP_SET_USER_AGENT IPC |
| Geolocation | JS navigator.geolocation override |
| Screenshots | WKSnapshotConfiguration + shared memory |
| Cookies | WKHTTPCookieStore native API |
| Mouse events (click/right-click/double-click/drag) | NSEvent dispatch via sendEvent: |
| Keyboard input | _executeEditCommand native API |
| Navigation | WKWebView loadRequest/loadHTMLString |
| JS evaluation | WKWebView evaluateJavaScript |

## Why not use Playwright's fork?

Playwright maintains a custom WebKit fork (Apache 2.0) that adds all the
missing features via a proprietary inspector protocol. We chose standard
WKWebView because:

1. **No build dependency** - WebKit takes 1-2 hours to build from source
   and needs ~40GB disk. Standard WKWebView ships with macOS.
2. **No maintenance burden** - Playwright rebases their 21K-line patch
   on every WebKit update. We'd need to do the same.
3. **Binary size** - The patched WebKit framework is ~500MB. Our host
   binary is 81KB.
4. **Startup time** - WKWebView is shared with the OS and pre-loaded.
   A custom framework must be loaded from disk.

The tradeoff: we get 80% of features at 0% maintenance cost. The missing
20% (request interception, CSP bypass, isolated worlds) can be added
later by either using Playwright's pre-built binary or forking WebKit.
