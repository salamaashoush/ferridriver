# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### BDD Framework — Complete Gherkin/Cucumber Spec Coverage

#### Core Test Runner Extensions
- **`StepStatus::Pending`** — new step status for not-yet-implemented steps, with `StepHandle::pending()` method
- **`TestId.line`** — optional source line number for `file:line` output in rerun/error reporting
- **Rerun reporter** (`--reporter rerun`) — writes failed test `file:line` locations to `@rerun.txt` for re-execution
- **Progress reporter** (`--reporter progress`) — minimal dot-based output (`.` pass, `F` fail, `S` skip, `?` flaky)
- **Config: `strict`** — when true, undefined/pending steps cause test failure
- **Config: `order`** — scenario execution order (`"defined"` or `"random"` / `"random:SEED"` for deterministic shuffle)
- **Config: `language`** — default Gherkin i18n language code (e.g., `"fr"`, `"de"`)
- **Config: `profiles`** — named configuration presets, merged via `--profile NAME`

#### BDD High-Priority Features
- **Custom parameter types** — `ParameterTypeRegistry` for defining `{custom}` placeholders in Cucumber expressions with `#[param_type]` proc macro and `defineParameterType()` in TypeScript
- **Regex step definitions** — `#[given(regex = r"^pattern$")]` in Rust, `Given(/regex/, cb)` in TypeScript
- **Snippet generation** — auto-generates step definition skeletons for undefined steps with correct `#[given/when/then]` attributes
- **Pending step status** — `StepError::pending()` in Rust, `Pending()` in TypeScript; non-strict mode treats undefined steps as pending (no failure)
- **Strict mode** — `--strict` flag: undefined and pending steps become failures instead of being silently accepted
- **Ambiguous step detection** — enhanced error messages showing all matching expressions with locations
- **i18n** — `--language` flag and `# language: xx` comment support for Gherkin keywords in 70+ languages
- **Asterisk (`*`) keyword** — generic step keyword works out of the box

#### BDD Medium-Priority Features
- **DataTable struct** — proper struct with `headers()`, `data_rows()`, `hashes()`, `rows_hash()`, `transpose()`, `cell()` methods; `Deref` to `[Vec<String>]` for backward compatibility
- **Doc string media types** — `"""json`, `"""yaml` content type hints parsed from doc strings
- **Named Examples blocks** — Scenario Outline examples with names show in output as `(ExampleName #1)` instead of `(Example #1)`
- **Scenario ordering** — `--order random[:SEED]` with deterministic Fisher-Yates shuffle
- **Usage reporter** (`--reporter usage`) — step expression call counts and total/avg duration statistics
- **BDD rerun reporter** (`--reporter rerun`) — writes failed scenario `file:line` to `@rerun.txt`

#### BDD Lower-Priority Features
- **Attachments API** — `world.attach(name, content_type, data)` and `world.log(text)` in step handlers; wired to `TestInfo` for report inclusion
- **Step composition** — `world.run_step("I click {string}")` to call steps from within other step handlers
- **Data table type transforms** — `FromDataTable` trait with `table.as_type::<T>()` for typed row conversion
- **Profiles** — `--profile NAME` deep-merges named config presets from `ferridriver.config.toml`
- **Cucumber Messages** (`--reporter messages`) — NDJSON event stream per the Cucumber Messages protocol

#### Reporters (all functional, tested simultaneously)
- `terminal` — Gherkin-formatted Feature > Scenario > Step hierarchy with colors
- `json` — machine-readable BDD results JSON
- `junit` — CI/CD-compatible JUnit XML
- `cucumber-json` — standard Cucumber JSON format for dashboards
- `usage` — step definition usage statistics table
- `rerun` — failed scenario locations for re-execution
- `messages` / `ndjson` — Cucumber Messages protocol NDJSON stream
- `progress` — dot-based minimal output
- `html` — self-contained HTML report with inline screenshots

#### TypeScript API — Cucumber-Compatible Surface
- **`Given`/`When`/`Then`/`Step`** — accept `string` (Cucumber expression with type inference) or `RegExp`, with optional `{ timeout }` options
- **`defineStep`** — keyword-agnostic alias (Cucumber compat)
- **`Before`/`After`** — accept `callback`, `string` tags, or `{ tags, name, timeout }` options
- **`BeforeStep`/`AfterStep`** — per-step hooks with same overload patterns
- **`BeforeAll`/`AfterAll`** — global lifecycle hooks
- **`defineParameterType`** — Cucumber-style `{ name, regexp, transformer }` object or `(name, regex)` shorthand
- **`setDefaultTimeout`** — global step timeout
- **`setWorldConstructor`** — Cucumber compat shim (no-op; ferridriver uses Page-first design)
- **`Status`** enum — `PASSED`, `FAILED`, `PENDING`, `SKIPPED`, `UNDEFINED`, `AMBIGUOUS`, `UNKNOWN`
- **`DataTable`** class — `raw()`, `rows()`, `hashes()`, `rowsHash()`, `transpose()`
- **`Pending(message?)`** — mark steps as not yet implemented
- **`version`** constant
- **Type inference** — `Given('I have {int} {string}', (page, count, item) => {})` infers `count: number`, `item: string`

#### NAPI Wiring
- Custom parameter types registered from TypeScript via `defineParameterType()`
- BeforeStep/AfterStep hooks wired through NAPI to Rust hook registry
- Per-step timeout passed through NAPI
- All reporters available via config (`reporter: ['terminal', 'cucumber-json', 'usage']`)
- i18n language config wired to Gherkin parser

#### New Feature Test Files
- `asterisk_keyword.feature` — `*` keyword as step prefix
- `background.feature` — Background steps before each scenario
- `but_keyword.feature` — `But` keyword for negative assertions
- `comments.feature` — `#` comment lines
- `data_tables.feature` — inline data tables
- `descriptions.feature` — free-form descriptions on Feature/Scenario/Rule
- `doc_strings.feature` — multi-line doc string content
- `i18n_french.feature` — French Gherkin keywords (`Soit`, `Alors`, `Et`)
- `named_examples.feature` — named Examples blocks in Scenario Outlines
- `pending_steps.feature` — undefined steps as pending (non-strict mode)
- `rule_keyword.feature` — Gherkin 6+ Rule keyword grouping
- `tag_expressions.feature` — complex tag filtering
