# [DONE] Feature: Allure Reporter

## Context
Allure is the de facto standard enterprise test reporting framework used by QA teams worldwide. It provides a rich dashboard with test history, categories, timelines, and trend analysis. Adding Allure output makes ferridriver immediately usable in enterprise CI/CD pipelines that already have Allure Report infrastructure. Teams get familiar reporting without migration cost.

## Design

### Architecture
| Crate/Package | Role |
|---|---|
| `ferridriver-test` | New `AllureReporter` implementing the `Reporter` trait |
| `ferridriver-cli` | `--reporter allure` flag |
| `packages/ferridriver-test` | `reporter: ['allure']` config option |

### Core Changes (ferridriver-test)
- New file `crates/ferridriver-test/src/reporter/allure.rs`:
  - Implements the `Reporter` trait (same as `html.rs`, `json.rs`, etc.).
  - Outputs Allure 2.x results format: one JSON file per test case + attachments.

- Allure result format (per test):
  ```json
  {
    "uuid": "unique-id",
    "historyId": "stable-hash-of-test-id",
    "name": "test name",
    "fullName": "file > suite > test name",
    "status": "passed|failed|broken|skipped",
    "statusDetails": { "message": "error msg", "trace": "stack trace" },
    "stage": "finished",
    "steps": [
      { "name": "step name", "status": "passed", "start": 1234, "stop": 5678 }
    ],
    "attachments": [
      { "name": "screenshot", "source": "uuid-attach.png", "type": "image/png" }
    ],
    "parameters": [
      { "name": "browser", "value": "chromium" }
    ],
    "labels": [
      { "name": "suite", "value": "Login Tests" },
      { "name": "tag", "value": "smoke" },
      { "name": "severity", "value": "critical" }
    ],
    "links": [
      { "name": "JIRA-1234", "url": "https://jira.example.com/JIRA-1234", "type": "issue" }
    ],
    "start": 1234567890,
    "stop": 1234567999
  }
  ```

- `AllureReporter` implementation:
  - On `TestStarted`: create result object, record start time.
  - On `StepStarted`/`StepFinished`: append to `steps` array.
  - On `TestFinished`:
    - Set status based on `TestStatus` -> Allure status mapping:
      - `Passed` -> `"passed"`, `Failed` -> `"failed"`, `TimedOut` -> `"broken"`, `Skipped` -> `"skipped"`, `Flaky` -> `"passed"` + flaky label.
    - Copy attachments (screenshots, videos, traces) to allure results dir.
    - Write `{uuid}-result.json`.
  - On `finalize`:
    - Write `environment.properties` (browser, OS, version).
    - Write `categories.json` (error classification rules).

- Labels auto-populated from test metadata:
  - `suite` from `TestId::suite`.
  - `tag` from `TestAnnotation::Tag`.
  - `severity` from `TestAnnotation::Info { type: "severity" }`.
  - `owner` from `TestAnnotation::Info { type: "owner" }`.

- Links auto-populated from `TestAnnotation::Info { type: "issue" }`.

### BDD Integration (ferridriver-bdd)
- BDD steps become Allure steps automatically.
- Feature name -> suite label.
- Scenario tags -> Allure tags.
- `@severity(critical)` -> severity label.

### NAPI + TypeScript (ferridriver-node, packages/ferridriver-test)
- Config: `reporter: [['allure', { outputDir: 'allure-results' }]]`.
- Options:
  - `outputDir`: directory for results (default: `allure-results`).
  - `suiteTitle`: override suite name.

### CLI (ferridriver-cli)
- `--reporter allure` enables the Allure reporter.
- Reporter options via config file: `[[reporter]] name = "allure" [reporter.options] output_dir = "allure-results"`.

### Component Testing (ferridriver-ct-*)
- Works identically. CT tests produce Allure results like any other test.

## Implementation Steps
1. Create `crates/ferridriver-test/src/reporter/allure.rs`.
2. Implement `Reporter` trait with Allure JSON output.
3. Implement Allure step recording from `StepStarted`/`StepFinished` events.
4. Implement attachment copying (screenshots, videos).
5. Generate `environment.properties` and `categories.json`.
6. Map `TestAnnotation` to Allure labels and links.
7. Register `"allure"` in the reporter factory in `reporter/mod.rs`.
8. Add integration with `allure serve` — verify output is compatible.
9. Test with real test suite.

## Key Files
| File | Action |
|---|---|
| `crates/ferridriver-test/src/reporter/allure.rs` | Create |
| `crates/ferridriver-test/src/reporter/mod.rs` | Modify — register allure reporter |
| `crates/ferridriver-test/src/config.rs` | Verify — reporter config supports options |

## Verification
- Unit test: `AllureReporter` produces valid JSON for a passed/failed/skipped test.
- Unit test: steps are correctly nested in the output.
- Integration test: run test suite with `--reporter allure`, verify `allure-results/` dir contains expected files.
- Integration test: `allure serve allure-results` opens dashboard with correct data.
- Verify attachments (screenshots) appear in Allure report.
- Verify BDD scenarios map to Allure tests with step details.
- Verify `environment.properties` contains browser and OS info.
