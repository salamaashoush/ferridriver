# [DONE] Feature: Parameterized Tests (test.each)

## Context
Data-driven testing is one of the most common patterns: run the same test logic with different inputs. Without `test.each()`, users duplicate test bodies or write manual loops that lose individual test identity. Playwright, Jest, and Vitest all support `test.each()` with template literal table syntax.

## Design

### Architecture
| Crate/Package | Role |
|---|---|
| `ferridriver-test` | Core parameterization: expand `TestCase` into N instances |
| `ferridriver-test-macros` | `#[ferritest_each]` proc macro for Rust |
| `ferridriver-bdd` | Scenario Outline with Examples table (already exists in Gherkin) |
| `packages/ferridriver-test` | `test.each()` and `describe.each()` TS API |

### Core Changes (ferridriver-test)
- New `Parameterized` variant on `TestCase` or a pre-discovery expansion step:
  - Option A (expansion at discovery): each parameter set creates a separate `TestCase` with a unique name (e.g., `"login with admin"`, `"login with guest"`). This is simpler and integrates naturally with filtering, retries, and reporting.
  - **Go with Option A** — expand at registration time, no runtime changes needed.
- `TestId` name includes parameter values: `"test name (param1, param2)"`.
- Each expanded test is fully independent: can be filtered, retried, reported individually.

### Rust API (ferridriver-test-macros)
- Macro: `#[ferritest_each(data = [(1, "a"), (2, "b")])]`
  ```rust
  #[ferritest_each(data = [("admin", "admin@example.com"), ("guest", "guest@example.com")])]
  async fn login(pool: FixturePool, role: &str, email: &str) {
    // test body using role and email
  }
  ```
- The proc macro expands into N `inventory::submit!` calls, one per data row.
- Name format: `login (admin, admin@example.com)`, `login (guest, guest@example.com)`.

### NAPI + TypeScript (ferridriver-node, packages/ferridriver-test)
- `test.each()` API:
  ```ts
  test.each([
    { role: 'admin', email: 'admin@example.com' },
    { role: 'guest', email: 'guest@example.com' },
  ])('login as $role', async ({ page }, { role, email }) => {
    // ...
  });
  ```
- `describe.each()` for parameterized suites.
- Template literal table syntax:
  ```ts
  test.each`
    role       | email
    ${'admin'} | ${'admin@example.com'}
    ${'guest'} | ${'guest@example.com'}
  `('login as $role', async ({ page }, { role, email }) => { ... });
  ```
- Implementation: `test.each()` returns a function that registers N tests in the registry.

### BDD Integration (ferridriver-bdd)
- Gherkin already supports this via `Scenario Outline` + `Examples` table.
- No additional BDD work needed — this is already implemented in the parser.
- Verify that each example row creates a separate test entry in the plan.

### CLI (ferridriver-cli)
- No new flags. Parameterized tests appear as individual tests in `--list` output.
- `--grep` can filter by parameter values in the test name.

### Component Testing (ferridriver-ct-*)
- `test.each()` works in CT mode — test different prop combinations:
  ```ts
  test.each([{ size: 'sm' }, { size: 'lg' }])('button $size', async ({ mount }, { size }) => {
    await mount(Button, { props: { size } });
  });
  ```

## Implementation Steps
1. Implement `test.each()` in `packages/ferridriver-test/src/test.ts` — registers N tests.
2. Implement `describe.each()` similarly.
3. Support `$variable` interpolation in test names.
4. Support tagged template literal table syntax.
5. Create `#[ferritest_each]` proc macro in `ferridriver-test-macros`.
6. Ensure expanded tests have unique `TestId` with parameter values in name.
7. Verify BDD `Scenario Outline` already creates per-example test entries.
8. Test filtering: `--grep "admin"` matches only admin parameterization.

## Key Files
| File | Action |
|---|---|
| `packages/ferridriver-test/src/test.ts` | Modify — add `test.each()`, `describe.each()` |
| `crates/ferridriver-test-macros/src/lib.rs` | Modify — add `#[ferritest_each]` |
| `crates/ferridriver-test/src/discovery.rs` | Verify — expanded tests registered correctly |

## Verification
- TS test: `test.each([1,2,3])('test %s', ...)` creates 3 separate tests in the plan.
- TS test: `$variable` interpolation works in test names.
- Rust test: `#[ferritest_each]` expands into correct number of inventory submissions.
- Verify `--list` shows each parameterized test as a separate entry.
- Verify `--grep "admin"` filters to only the admin parameterization.
- Verify retries work independently per parameterized test.
