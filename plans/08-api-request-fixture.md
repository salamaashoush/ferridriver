# Feature: API Request Fixture

## Context
Many tests need to set up data via API calls before browser interaction, or verify server state after actions. An API request fixture provides a high-level HTTP client that shares cookies with the browser context, eliminating the need for external HTTP libraries. This is Playwright's `request` fixture — clean API testing alongside E2E.

## Design

### Architecture
| Crate/Package | Role |
|---|---|
| `ferridriver` | `APIRequestContext` — HTTP client with cookie jar integration |
| `ferridriver-test` | `request` fixture in the fixture pool |
| `ferridriver-bdd` | API steps: `When I send a GET request to {string}` |
| `ferridriver-cli` | No new flags |
| `packages/ferridriver-test` | `request` fixture available in test callbacks |

### Core Changes (ferridriver)
- New module `crates/ferridriver/src/api_request.rs`:
  - `APIRequestContext` struct:
    - Built on `reqwest::Client` with cookie jar.
    - Methods: `get`, `post`, `put`, `delete`, `patch`, `head`, `fetch` (generic).
    - Each returns `APIResponse` with `status()`, `json()`, `text()`, `headers()`, `body()`.
  - Configuration:
    - `base_url: Option<String>` — prepended to relative URLs.
    - `extra_http_headers: HashMap<String, String>` — default headers.
    - `http_credentials: Option<HttpCredentials>` — basic auth.
    - `ignore_https_errors: bool`.
    - `timeout: Duration`.
  - Cookie integration: can import cookies from a `StorageState` or browser context.
  - `APIResponse`:
    ```rust
    pub struct APIResponse {
      pub status: u16,
      pub headers: HeaderMap,
      pub url: String,
      body: Bytes,
    }
    impl APIResponse {
      pub fn json<T: DeserializeOwned>(&self) -> Result<T, Error>;
      pub fn text(&self) -> String;
      pub fn ok(&self) -> bool;  // 200-299
    }
    ```
  - Multipart support: `post(url).multipart(form)` with `FormData` builder.
  - Request builder pattern: `context.post(url).header("X-Token", token).json(&body).send()`.

### Core Changes (ferridriver-test)
- Register `request` as a built-in fixture in `fixture.rs`:
  - Scope: per-test (each test gets its own `APIRequestContext`).
  - Config: inherits `base_url` from `TestConfig`.
  - Teardown: `dispose()` — clears cookie jar, aborts pending requests.
- `FixturePool` gets a `request()` accessor.

### BDD Integration (ferridriver-bdd)
- New step definitions in `crates/ferridriver-bdd/src/steps/api.rs`:
  - `When I send a GET request to {string}` — stores response in world.
  - `When I send a POST request to {string} with body:` (doc string) — JSON body.
  - `Then the response status should be {int}`.
  - `Then the response body should contain {string}`.
  - `Then the response JSON at {string} should equal {string}` (JSON path).
- `World` gets an `api_response: Option<APIResponse>` field.

### NAPI + TypeScript (ferridriver-napi, packages/ferridriver-test)
- Add `request` to `TestFixtures` interface.
- NAPI: expose `APIRequestContext` as a JS class with async methods.
- Usage: `test('api', async ({ request }) => { const res = await request.get('/api/users'); })`.

### CLI (ferridriver-cli)
- No new flags. API context configured via `TestConfig::base_url` and `TestConfig::extra_http_headers`.

### Component Testing (ferridriver-ct-*)
- `request` fixture available in CT tests for mocking API setup before component mount.

## Implementation Steps
1. Add `reqwest` dependency to `ferridriver/Cargo.toml` (with `json`, `multipart`, `cookies` features).
2. Create `crates/ferridriver/src/api_request.rs` with `APIRequestContext` and `APIResponse`.
3. Implement request builder pattern with `get/post/put/delete/patch/head`.
4. Add cookie import from `StorageState`.
5. Register `request` fixture in `ferridriver-test/src/fixture.rs`.
6. Create BDD API steps in `ferridriver-bdd/src/steps/api.rs`.
7. Add NAPI bindings for `APIRequestContext`.
8. Add `request` to TS `TestFixtures` interface.
9. Write tests.

## Key Files
| File | Action |
|---|---|
| `crates/ferridriver/src/api_request.rs` | Create |
| `crates/ferridriver/src/lib.rs` | Modify — export `api_request` module |
| `crates/ferridriver-test/src/fixture.rs` | Modify — register `request` fixture |
| `crates/ferridriver-bdd/src/steps/api.rs` | Create |
| `crates/ferridriver-napi/src/lib.rs` | Modify — expose `APIRequestContext` |
| `packages/ferridriver-test/src/test.ts` | Modify — add `request` to fixtures |

## Verification
- Unit test: `APIRequestContext` sends GET/POST to httpbin.org or a local mock server.
- Unit test: `APIResponse::json()` deserializes correctly.
- Integration test: login via API, extract cookies, verify browser context has them.
- BDD test: feature with API steps sets up data and verifies response status.
- TS test: `request.get('/api/health')` returns 200.
