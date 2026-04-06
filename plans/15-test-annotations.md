# [DONE] Feature: test.info() Annotations + test.fixme(condition)

## Context
Tests need metadata beyond pass/fail: links to issue trackers, environment-specific known failures, severity levels. `test.info().annotations` lets tests attach structured metadata that reporters can display. `test.fixme(condition, reason)` is a conditional skip that marks the test as a known issue, unlike `.skip()` which is silent.

## Design

### Architecture
| Crate/Package | Role |
|---|---|
| `ferridriver-test` | `TestAnnotation::Info`, conditional `Fixme`, `TestInfo` annotation API |
| `ferridriver-test-macros` | `#[fixme(condition)]` attribute |
| `ferridriver-bdd` | `@fixme` tag with optional condition |
| `ferridriver-cli` | No new flags |
| `packages/ferridriver-test` | `test.info().annotations`, `test.fixme()` API |

### Core Changes (ferridriver-test)
- Extend `TestAnnotation` enum in `model.rs`:
  ```rust
  pub enum TestAnnotation {
    Skip { reason: Option<String> },
    Slow,
    Fixme { reason: Option<String>, condition: Option<String> },  // MODIFIED: add condition
    Fail,
    Tag(String),
    Info { type_name: String, description: String },  // NEW: key-value metadata
  }
  ```
- `TestAnnotation::Info` examples:
  - `Info { type_name: "issue", description: "JIRA-1234" }` — link to issue tracker.
  - `Info { type_name: "severity", description: "critical" }`.
  - `Info { type_name: "owner", description: "team-auth" }`.

- Conditional `Fixme`:
  - `Fixme` with a `condition` field: evaluate at runtime.
  - Condition is a string expression evaluated against test context (platform, browser, etc.).
  - Simple condition syntax: `"linux"`, `"webkit"`, `"!chromium"`, `"ci"`.
  - When condition matches: test is skipped and marked as fixme (distinct from skip in reports).
  - When condition doesn't match: test runs normally.

- `TestInfo` API additions:
  ```rust
  impl TestInfo {
    pub fn annotate(&self, type_name: &str, description: &str);
    pub fn annotations(&self) -> Vec<TestAnnotation>;
  }
  ```

### BDD Integration (ferridriver-bdd)
- `@fixme` tag: marks scenario as fixme (always).
- `@fixme(linux)` tag: conditional fixme on Linux.
- `@issue(JIRA-1234)` tag: parsed into `Info { type_name: "issue", description: "JIRA-1234" }`.
- Tag syntax: `@key(value)` pattern parsed into `Info` annotations.

### NAPI + TypeScript (ferridriver-napi, packages/ferridriver-test)
- `test.info()` returns `TestInfo` with annotation methods:
  ```ts
  test('example', async ({ page }) => {
    test.info().annotations.push({ type: 'issue', description: 'GH-456' });
    // ...
  });
  ```
- `test.fixme(condition, reason)`:
  ```ts
  test('broken on webkit', async ({ page }) => {
    test.fixme(browserName === 'webkit', 'WebKit does not support this API');
    // ...
  });
  ```
- When `test.fixme()` is called with a truthy condition, the test is immediately skipped.

### CLI (ferridriver-cli)
- No new flags. Annotations are test-level, not CLI-level.

### Reporter Integration
- HTML reporter: show annotations in test detail view (table of type -> description).
- JSON reporter: include annotations array in test results.
- Terminal reporter: show fixme reason in skip message.
- `@issue` annotations: rendered as clickable links in HTML if the description matches a URL pattern.

### Component Testing (ferridriver-ct-*)
- No CT-specific changes. Annotations work identically.

## Implementation Steps
1. Add `Info` variant to `TestAnnotation` in `model.rs`.
2. Add `condition` field to `Fixme` variant.
3. Add `annotate()` and `annotations()` methods to `TestInfo`.
4. Implement conditional fixme evaluation in `Worker::run_test()`.
5. Parse `@fixme(condition)` and `@key(value)` tags in BDD `filter.rs`.
6. Add `test.fixme(condition, reason)` to TS API in `test.ts`.
7. Add `test.info().annotations` to TS API.
8. Update HTML reporter to display annotations.
9. Update JSON reporter to include annotations.
10. Add `#[fixme(condition)]` to proc macro.

## Key Files
| File | Action |
|---|---|
| `crates/ferridriver-test/src/model.rs` | Modify — extend `TestAnnotation` |
| `crates/ferridriver-test/src/worker.rs` | Modify — conditional fixme evaluation |
| `crates/ferridriver-bdd/src/filter.rs` | Modify — parse `@key(value)` tags |
| `packages/ferridriver-test/src/test.ts` | Modify — `test.fixme()`, `test.info()` |
| `crates/ferridriver-test/src/reporter/html.rs` | Modify — display annotations |
| `crates/ferridriver-test/src/reporter/json.rs` | Modify — include annotations |

## Verification
- Unit test: `Info` annotation round-trips through serialization.
- Unit test: conditional fixme with matching condition -> test skipped with fixme status.
- Unit test: conditional fixme with non-matching condition -> test runs normally.
- BDD test: `@fixme(linux)` skips on Linux, runs on macOS.
- BDD test: `@issue(JIRA-1234)` appears in HTML report as annotation.
- TS test: `test.info().annotations` contains pushed annotations after test runs.
- HTML report: annotations displayed in test detail panel.
