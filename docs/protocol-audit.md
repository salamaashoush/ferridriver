# Protocol coverage audit

This records the protocol boundary that the current scripting API actually
implements. It is based on the [WebDriver BiDi specification](https://www.w3.org/TR/webdriver-bidi/),
[Appium session capabilities](https://appium.io/docs/en/3.2/guides/caps/), and
[Apple's Safari WebDriver documentation](https://developer.apple.com/documentation/webkit/about-webdriver-for-safari).

Ferridriver accepts a WebDriver HTTP endpoint through `BrowserType.connect`.
It creates one W3C session with `webSocketUrl: true`, preserves caller supplied
standard and namespaced capabilities, and attaches to the returned BiDi socket.
This is the low-latency path used for Chromium, Firefox, WebKit, and Appium
servers that expose BiDi. It avoids creating a second session after the HTTP
handshake.

The implementation deliberately reports a typed unsupported error when a
server creates only a Classic WebDriver session. The Playwright-shaped browser
and page API is backed by BiDi, so pretending that a Classic session supports
events, browsing-context lifecycle, network interception, or script handles
would make tests pass while silently dropping behavior.

The remaining device requirement is therefore operational rather than a
capability merge: a Safari or Appium driver must expose the negotiated BiDi
socket. A live Safari, XCUITest, or UiAutomator2 session is not available in
the current gate environment, so those paths still need device-backed tests
before they can be called complete.

The scripting connection shape for a BiDi-capable driver is:

```ts
const browser = await webkit().connect('http://127.0.0.1:4444', {
  headers: { authorization: `Bearer ${process.env.APPIUM_TOKEN}` },
  timeout: 30_000,
  capabilities: {
    platformName: 'iOS',
    'appium:options': {
      automationName: 'Safari',
      deviceName: 'example-device',
    },
  },
});
```

Android uses the same connection shape with `platformName: 'Android'` and
driver-specific `appium:options` values. Ferridriver preserves those
namespaced capabilities and forces `webSocketUrl: true` because its page API
is implemented over the returned BiDi socket. A Classic-only driver must use a
BiDi-enabled configuration or remains unsupported by design.
