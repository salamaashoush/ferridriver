# Feature: Code Generation (Interactive Recorder)

## Context
Playwright's `codegen` command opens a headed browser and records user interactions into test code. This is the fastest way for new users to create tests and for experienced users to capture complex interaction sequences. It also serves as a selector discovery tool, showing the best selector for each element.

## Design

### Architecture
| Crate/Package | Role |
|---|---|
| `ferridriver` | Headed browser launch, CDP event interception, selector generation |
| `ferridriver-cli` | `ferridriver codegen <url>` command |
| `ferridriver-test` | None (codegen is a standalone tool) |
| `packages/ferridriver-test` | `ferridriver-test codegen <url>` TS wrapper |

### Core Changes (ferridriver)
- New module `crates/ferridriver/src/codegen/`:
  - `recorder.rs` — `Recorder` struct: subscribes to CDP input events, builds action list.
    - Intercepts: `click`, `dblclick`, `fill` (input change), `keypress`, `navigation`, `select`.
    - Uses `Input.dispatchMouseEvent` / `Input.dispatchKeyEvent` interception.
    - Injects a highlight overlay script into the page (shows element under cursor + best selector).
  - `selector.rs` — `SelectorGenerator`: given a DOM element, produce the best selector.
    - Priority: `data-testid` > `role + name` > `text=` > CSS id > CSS class chain.
    - Uses `DOM.describeNode` + `Accessibility.getFullAXTree` for role-based selectors.
  - `emitter.rs` — `CodeEmitter` trait + implementations:
    - `RustEmitter`: generates `#[ferritest]` async test with `page.click()`, `page.fill()`, etc.
    - `TypeScriptEmitter`: generates `test('...', async ({ page }) => { ... })`.
    - `GherkinEmitter`: generates `.feature` file with `When I click ...`, `And I fill ...`.
  - `Action` enum: `Click { selector }`, `Fill { selector, value }`, `Navigate { url }`, `Press { key }`, `Select { selector, value }`, `Assert { selector, assertion }`.

### Assertion Mode
- Toggle assertion mode with a toolbar button or keyboard shortcut.
- In assertion mode, clicking an element adds an assertion: `expect(page.locator('...')).toBeVisible()`.
- Shift+click adds text assertion: `expect(page.locator('...')).toHaveText('...')`.

### CLI (ferridriver-cli)
- New subcommand: `ferridriver codegen [OPTIONS] <URL>`.
  - `--output <format>`: `rust` (default), `typescript`, `gherkin`.
  - `--output-file <path>`: write to file instead of stdout.
  - `--viewport <WxH>`: set viewport size.
  - `--device <name>`: emulate device (e.g., "iPhone 13").
- Opens headed browser, navigates to URL, starts recording.
- Prints generated code to stdout in real-time as actions are recorded.
- Ctrl+C or closing the browser stops recording and outputs final code.

### BDD Integration
- `--output gherkin` generates `.feature` files with natural language steps.
- Maps recorded actions to existing BDD step definitions where possible.
- Falls back to generic steps for unmatched actions.

### Component Testing (ferridriver-ct-*)
- `ferridriver codegen --ct <component>`: mounts a component, then records interactions.
- Generates CT-specific test code with `mount()` call.

## Implementation Steps
1. Create `crates/ferridriver/src/codegen/mod.rs` — module structure.
2. Implement `Recorder` — CDP event subscription + action list building.
3. Implement `SelectorGenerator` — DOM analysis + selector ranking.
4. Implement `RustEmitter`, `TypeScriptEmitter`, `GherkinEmitter`.
5. Build highlight overlay JS (inject into page, show selector on hover).
6. Add `codegen` subcommand to `ferridriver-cli/src/cli.rs`.
7. Wire up CLI: launch headed browser, start recorder, stream code to stdout.
8. Implement assertion mode toggle.
9. Add `--device` emulation support.
10. Test with real websites.

## Key Files
| File | Action |
|---|---|
| `crates/ferridriver/src/codegen/mod.rs` | Create |
| `crates/ferridriver/src/codegen/recorder.rs` | Create |
| `crates/ferridriver/src/codegen/selector.rs` | Create |
| `crates/ferridriver/src/codegen/emitter.rs` | Create |
| `crates/ferridriver-cli/src/cli.rs` | Modify — add `Codegen` subcommand |
| `crates/ferridriver-cli/src/main.rs` | Modify — handle `Codegen` command |

## Verification
- Manual: `ferridriver codegen https://example.com` — verify browser opens, actions are recorded.
- Manual: click elements, fill inputs, navigate — verify generated Rust code compiles.
- Manual: switch to TypeScript output — verify valid TS test.
- Manual: switch to Gherkin output — verify valid `.feature` file.
- Manual: assertion mode — verify assertions are generated for clicked elements.
- Unit test: `SelectorGenerator` produces correct selectors for known DOM structures.
- Unit test: each `CodeEmitter` produces syntactically valid output for a sequence of actions.
