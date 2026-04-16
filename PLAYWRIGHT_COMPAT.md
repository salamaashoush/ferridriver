# Playwright API Compatibility Tracker

Known incompatibilities between ferridriver and Playwright's API that need to be fixed.

## Cookies (Context-only in Playwright)

- [ ] **Move cookie methods from Page to Context only** -- Playwright has cookies on `BrowserContext` only (`context.cookies()`, `context.addCookies()`, `context.clearCookies()`). ferridriver currently has them on both Page and Context. Page cookie methods should be removed and callers migrated to `page.context().cookies()` etc.
  - Affected files: `page.rs` (remove `cookies()`, `set_cookie()`, `delete_cookie()`, `clear_cookies()`, `add_cookies()`, `clear_cookies_filtered()`)
  - Callers to migrate: `ferridriver-mcp/src/tools/cookies.rs`, `ferridriver-mcp/src/server.rs`, `ferridriver/src/steps/cookie.rs`, `ferridriver-node/src/page.rs`
  - `page.context()` was added -- use it to access cookie API from page

- [ ] **`cookies(urls?)` URL filtering** -- Playwright's `context.cookies(urls?)` accepts optional URL strings to filter cookies by. ferridriver's `cookies()` returns all cookies with no filtering.

- [ ] **`clearCookies(options?)` with regex filters** -- Playwright supports `name: string | RegExp`, `domain: string | RegExp`, `path: string | RegExp` in clearCookies options. ferridriver currently only supports exact string match in `ClearCookieOptions`. Add regex support.

- [ ] **`sameSite` in CookieData** -- DONE: Added `same_site: Option<SameSite>` field with `Strict`/`Lax`/`None` enum. All backends updated.

- [ ] **`SetCookieParams.url` field** -- Playwright's `SetNetworkCookieParam` has a `url` field that auto-derives domain/path. The `SetCookieParams` struct was added but `url` is not yet processed by backends (need to parse URL to extract domain/path).

## Storage

- [ ] **`storageState` on Context, not just Page** -- Playwright has `context.storageState()` and `context.setStorageState()`. ferridriver only has these on Page. Should be on Context with Page delegating.

- [ ] **`storageState` indexedDB support** -- Playwright's `storageState({ indexedDB: true })` can include IndexedDB data. ferridriver doesn't support this.

## Page

- [x] **`page.context()`** -- DONE: Added `Page::with_context()` and `page.context()` returning `Option<&ContextRef>`. `ContextRef::new_page()` and `pages()` now pass context ref.

## BrowserContext

- [ ] **`context.storageState()`** -- Move from Page to Context (see Storage above)
- [ ] **`context.route()`** -- Playwright has request interception on context level
- [ ] **`context.setOffline()`** -- Exists in ferridriver but verify API match
- [ ] **`context.addInitScript()`** -- Exists but verify return type and behavior

## General

- [ ] **Review all Playwright API methods** for completeness across Page, Locator, BrowserContext, Browser, Frame
