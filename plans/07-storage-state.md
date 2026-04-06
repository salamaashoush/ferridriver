# Feature: Storage State Save/Load

## Context
Most web apps require authentication. Without storage state, every test must log in independently, wasting time and adding flakiness. Storage state serializes cookies + localStorage to JSON, letting tests reuse authenticated sessions. This is Playwright's recommended pattern for auth: log in once in global setup, save state, all tests start pre-authenticated.

## Design

### Architecture
| Crate/Package | Role |
|---|---|
| `ferridriver` | `context.storageState()` and `browser.newContext({ storageState })` APIs |
| `ferridriver-test` | `storageState` config, global setup integration |
| `ferridriver-bdd` | `@auth` tag pattern, beforeAll hook saves state |
| `ferridriver-cli` | No new flags (config-driven) |
| `packages/ferridriver-test` | `storageState` in config, `globalSetup` pattern |

### Core Changes (ferridriver)
- New struct `StorageState` in `crates/ferridriver/src/storage_state.rs`:
  ```rust
  pub struct StorageState {
    pub cookies: Vec<CookieParam>,
    pub origins: Vec<OriginStorage>,  // localStorage per origin
  }
  pub struct OriginStorage {
    pub origin: String,
    pub local_storage: Vec<KV>,
  }
  pub struct KV { pub name: String, pub value: String }
  ```
- `Page::storage_state() -> StorageState`:
  - Cookies: `Network.getAllCookies` CDP command.
  - LocalStorage: `Runtime.evaluate` on each origin to dump `Object.entries(localStorage)`.
- `Page::set_storage_state(state: &StorageState)`:
  - Cookies: `Network.setCookies` with all cookies.
  - LocalStorage: `Runtime.evaluate` to set each key-value pair per origin.
- Serialization: `StorageState` derives `Serialize`/`Deserialize` — JSON round-trips cleanly.

### Core Changes (ferridriver-test)
- Add to `TestConfig`:
  ```rust
  pub storage_state: Option<String>,  // path to JSON file
  ```
- Add to `ProjectConfig`:
  ```rust
  pub storage_state: Option<String>,
  ```
- In `Worker`: when creating a page for a test, if `storage_state` is set:
  1. Load the JSON file.
  2. Deserialize into `StorageState`.
  3. Call `page.set_storage_state(&state)` before handing the page to the test.
- In fixture system: `storageState` can be a fixture that auto-loads from config path.

### BDD Integration (ferridriver-bdd)
- Pattern: `@auth` tagged scenarios use a pre-saved storage state.
- Global setup hook or `Before(@auth)` hook: check if `auth.json` exists, if not run login scenario.
- BDD steps for explicit state management:
  - `Given I save the storage state to {string}`
  - `Given I load the storage state from {string}`

### NAPI + TypeScript (ferridriver-napi, packages/ferridriver-test)
- Config: `storageState: './auth.json'` in `ferridriver.config.ts`.
- API: `page.context().storageState()` returns JS object or saves to file.
- Global setup: `globalSetup: './global-setup.ts'` that logs in and saves state.

### CLI (ferridriver-cli)
- No new CLI flags. Storage state is set via config file or programmatically.
- Could add `--storage-state <path>` as convenience override.

### Component Testing (ferridriver-ct-*)
- CT typically doesn't need auth, but storage state can set localStorage for component state.

## Implementation Steps
1. Create `crates/ferridriver/src/storage_state.rs` with `StorageState` struct.
2. Implement `Page::storage_state()` using CDP `Network.getAllCookies` + JS evaluation.
3. Implement `Page::set_storage_state()` using CDP `Network.setCookies` + JS evaluation.
4. Add `storage_state` to `TestConfig` and `ProjectConfig`.
5. In `Worker`, apply storage state before each test when configured.
6. Add BDD steps: save/load storage state.
7. Add NAPI bindings for `storageState()` and config.
8. Write integration test: login -> save state -> new context with state -> verify logged in.

## Key Files
| File | Action |
|---|---|
| `crates/ferridriver/src/storage_state.rs` | Create |
| `crates/ferridriver/src/page.rs` | Modify — add `storage_state()`, `set_storage_state()` |
| `crates/ferridriver-test/src/config.rs` | Modify — add `storage_state` field |
| `crates/ferridriver-test/src/worker.rs` | Modify — apply storage state on page creation |
| `crates/ferridriver-bdd/src/steps/` | Modify — add storage state steps |

## Verification
- Unit test: `StorageState` round-trips through JSON (serialize -> deserialize -> equal).
- Integration test: navigate to site, set cookies/localStorage, call `storage_state()`, verify JSON contains expected data.
- Integration test: load state into fresh page, verify cookies and localStorage are present.
- E2E test: global setup logs into a test app, saves state; test starts with state, verifies authenticated.
