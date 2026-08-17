# Playwright compatibility harness

A bug-finding instrument, not a product feature. It runs **upstream
Playwright suites, byte-for-byte unmodified**, against ferridriver and
treats every failure as a ferridriver compat bug until it is recorded
below as an intentional divergence.

There is no "Playwright compatibility mode" being shipped to users. The
only user-visible surface any of this adds is one config key
(`[test].moduleAliases`) and one CLI flag (`--module-alias`).

## Running it

```bash
just compat                     # everything (needs network for 3 of 5 examples)
just compat --offline           # only the examples with a local app
just compat --example todomvc
just compat-update              # re-record the baseline after fixing a gap
```

The script is `scripts/playwright-compat.sh`. It clones
`microsoft/playwright` into `/tmp/playwright` if it is not there
(override with `PLAYWRIGHT_REPO`), verifies the corpus against
`tests/compat/corpus.sha256`, generates a ferridriver config per example
**outside** the corpus, runs each suite, and diffs the result against
`tests/compat/baseline.json`. A test that used to pass and no longer does
fails the gate.

`COMPAT_BACKEND=bidi just compat` runs the corpus on another backend.

## The corpus

`/tmp/playwright/examples/*/tests` — Playwright's own example projects.
Genuine user-style specs, not Playwright's internal suite. 32 files,
checksum-pinned.

Playwright's own `tests/page/*.spec.ts` are deliberately **not** a target:
they import `./pageTest`, a large private fixture harness, and only 2 of
hundreds import solely from `@playwright/test`. The realistic target is a
user's suite.

| example | app | how it runs |
|---|---|---|
| `todomvc` | demo.playwright.dev | network |
| `svgomg` | demo.playwright.dev | network |
| `mock-battery` | local static dir | hermetic (`webServer.staticDir`) |
| `mock-filesystem` | local static dir | hermetic (`webServer.staticDir`) |
| `github-api` | api.github.com | discovery only — the suite creates and deletes a real GitHub repo |

## Results (2026-08-08)

**35 of 36 executed tests pass** — the same result Playwright 1.x itself
produces on this corpus, verified by running the identical specs under
`@playwright/test` with the same Chromium.

| example | ferridriver | upstream Playwright |
|---|---|---|
| todomvc | 24/24 | 24/24 |
| svgomg | 6/6 | 6/6 |
| mock-battery | 4/4 | 4/4 |
| mock-filesystem | 1/2 | 1/2 |
| github-api | 2 collected | not executed |

Baseline at the start of this work: **30 tests ran, 30 failed.**

## Module aliasing

Upstream specs `import { test, expect } from '@playwright/test'`.
`@playwright/test` and `playwright/test` are now served NATIVELY — the
same module `@ferridriver/test` serves — so a suite needs no config at
all to link against the runtime's test surface. Parity belongs to the
binary, not to a setting.

Aliasing remains for everything else a suite might import under its own
name:

```toml
[test.moduleAliases]
"playwright" = "ferridriver"
```

Aliases reach both the runtime module loader
(`bindings/native_modules.rs`) and the rolldown externals (`bundle.rs`),
which read the same list, and are folded into the bytecode cache key. An
alias target must be a native module, and an alias may not REDIRECT a
native specifier — though spelling out one that already resolves there
(`"@playwright/test" = "@ferridriver/test"`) stays accepted as the no-op
it is, so configs written before this kept working.

## Divergence ledger

Every failure the harness surfaced is either fixed at the wire or
recorded here with the reason. No silent skips.

### Fixed

| gap | tests | fix |
|---|---|---|
| `test.extend({ page: async ({ page }, use) => … })` rejected as a dependency cycle | 24 | A dependency named the same as the fixture declaring it now resolves to that fixture's SUPER — the previous registration in the extend chain — mirroring `FixturePool.resolve`. `bindings/fixture_graph.rs`; the glue crate's pool-request computation walks the same graph. Overrides also inherit `scope`/`auto`/`option` from the registration they shadow, and contradicting them is an error, as in `_appendFixtureList`. |
| CommonJS `require('@playwright/test')` died at load | 5 | rolldown lowers a `require()` of an external specifier to its `__require` helper, which defers to a global `require` when one exists. `globalThis.require` now serves the native specifiers (and only those). Its export map is the same object the ESM path exports, so the two can never drift. |
| `webServer` with only a `staticDir`/`port` left `page.goto('/')` unresolved | 6 | `baseURL` falls back to `FERRIDRIVER_BASE_URL`, the channel the web-server startup already exported — the same shape as Playwright's `PLAYWRIGHT_TEST_BASE_URL` plus its `baseURL` option fixture. |
| `expect(locator).toHaveText([...])` / `toContainText([...])` rejected the array form | 3 | Implemented `to.have.text.array` / `to.contain.text.array` in the expect core, including `_matchSequentially` and `normalizeWhiteSpace` semantics. The pre-existing ferridriver-only `toHaveTexts`/`toContainTexts` now delegate to it instead of carrying a second, `document.querySelectorAll`-based implementation that ignored frames and engine selectors. |
| `toBeChecked()` was false for anything but `<input>` | 2 | `is_checked` reads through the injected `getChecked`, which now retargets `follow-label` first and understands every `aria-checked` role. A non-checkable element is an error, not `false`. |
| `fileChooser.setFiles({ name, mimeType, buffer })` failed to deserialise | 1 | `FilePayload.buffer` goes through the shared byte extractor, so the node-compat `Buffer` class (not a `Uint8Array` subclass) is accepted alongside typed arrays. |
| exposed-function calls arrived out of order | 2 | CDP `Runtime.bindingCalled` was dispatched with `tokio::spawn` per call, so the multi-threaded scheduler reordered them. Calls now run on one serial task fed in wire order; only the result round-trip stays spawned. (WebKit and BiDi already awaited inline.) |
| `toMatchAriaSnapshot` rejected every partial template | 1 | The hand-rolled line-subsequence comparison is gone. Rust parses nothing: the YAML is parsed by an on-demand `aria-support` bundle and matched by Playwright's own `matchesExpectAriaTemplate`, mirroring Playwright's server-parse / in-page-match split. `expect(page).toMatchAriaSnapshot` was a substring test on rendered YAML; it now goes through the same matcher against `document.body`. |
| `fs.readFileSync` missing | 1 | The fs sandbox was async-only. The jail check was already synchronous, so `readFileSync` / `readFileBytesSync` / `existsSync` are the same path minus the await. **Decision: the async-only stance is dropped** — a spec that reads a fixture or a download synchronously is ordinary, and refusing it only breaks otherwise-portable suites. |
| reading `download.path()` was a sandbox violation | 1 | A download lands in a backend-owned temp dir, outside `script_root`. Files this process downloaded through a browser are now readable at their real path — and nothing else outside the root is. |
| an `<iframe>` chain resolved once, before the frame loaded | 1 | `retry_resolve!` hoisted `resolved()` out of the retry loop, so a `frameLocator` chain kept querying a stale frame for the whole deadline (and an unknown frame id falls back to the main document, so the selector silently matched the wrong page). It re-resolves per attempt now. |

### Intentional divergence

| behaviour | decision |
|---|---|
| `mock-filesystem` › *should display directory tree* fails | **Corpus defect, not a gap.** `expect(page.locator('#dir')).toContainText([...7 strings])` asserts an array against a locator matching ONE element; `to.contain.text.array` matches expectations against elements pairwise, so 7 expectations can never be satisfied by 1 element. Verified by running the identical spec under `@playwright/test`: it fails with a byte-identical expected/received diff. Our behaviour matches Playwright exactly. |
| `github-api` is collected, not executed | The suite creates and deletes a real repository under `$GITHUB_USER` with `$API_TOKEN`. The gate exercises discovery, bundling and registration; running it would mutate someone's GitHub account. |
| the compat configs set `retries = 1` | The `mock-battery` demo app loads `src/index.js` with `async`, so its `getBattery()` microtask races `styles.css`; when the script wins, the app throws on `document.styleSheets[0].insertRule` and renders nothing. That is the app's race — the page error and the correctly-installed mock are both observable — but it is real, and one retry is what Playwright's own example configs use. A genuine regression fails both attempts. |

### Not yet measured

- `playwright.config.ts` compatibility. ferridriver's config is
  `ferridriver.toml`; the harness generates one per example rather than
  reading the upstream config. Nothing in the corpus depends on it at
  runtime.
- Playwright's load-time fixture validation (`has unknown parameter
  "x"`). ferridriver validates the self-reference case, which is what
  distinguishes an override from a typo, but an unknown non-self
  dependency still resolves to `undefined` rather than failing the load.
- `test.extend` overriding an option fixture with `undefined` to restore
  the original default (`_appendFixtureList`'s `optionOverride` walk).
