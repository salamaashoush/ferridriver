# Recipes

Practical patterns lifted from real ferridriver suites. Each page is
self-contained and copy-paste ready.

| Recipe | What it covers |
|--------|----------------|
| [Login and saved auth state](/recipes/login-and-auth-state) | Sign in once, reuse the session across tests |
| [Network mocking](/recipes/network-mocking) | `route()` to fulfill / abort / continue; HAR replay |
| [File upload and download](/recipes/file-upload-download) | `setInputFiles`, capturing downloads via the `download` event |
| [Cookies and storage](/recipes/cookies-and-storage) | `addCookies`, `storageState`, per-context overrides |
| [Multiple tabs and windows](/recipes/multiple-tabs) | New pages, popup handling, switching active tab |
| [Mobile emulation](/recipes/mobile-emulation) | Viewport, user agent, touch, device scale factor |
| [Screenshots and traces](/recipes/screenshots-and-traces) | Full page, element, masking, Playwright-compatible traces |
| [CI on GitHub Actions](/recipes/ci-github-actions) | Sharded matrix, JUnit upload, video on failure |

Each Rust snippet assumes a `#[ferritest]` body unless noted; each
TypeScript snippet assumes `@ferridriver/node`. The browser calls are
identical across languages — only the harness differs.
