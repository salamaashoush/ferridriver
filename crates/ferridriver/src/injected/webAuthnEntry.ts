/**
 * Standalone entry for the WebAuthn virtual authenticator (Playwright
 * injected/src/webAuthn.ts, vendored verbatim as ./webAuthn.ts).
 *
 * Compiled to dist/webauthn.min.js and installed as a context init script
 * by `BrowserContext::credentials().install()`; it answers
 * navigator.credentials through the `__pwWebAuthnBinding` binding.
 */

import { inject } from './webAuthn';

inject(globalThis);
