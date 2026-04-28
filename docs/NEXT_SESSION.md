# Next session — Cluster 4 (generic + asymmetric matchers, §7.11 – §7.16)

Cluster 3 (TestInfo helpers) is shipped. Cluster 4 is the matcher
work: bring `expect()` up to Playwright/Jest parity by adding the
generic value matchers (toBe, toEqual, toContain, toMatchObject,
toThrow, etc.), the asymmetric-matcher namespace (`expect.any`,
`expect.objectContaining`, …), `.resolves` / `.rejects` modifiers,
TS-side exposure of the existing `.soft` / `.poll`, `expect.extend`
for custom matchers, and `toBeOK` for `APIResponse`.

Two-commit landing is acceptable per the original cluster guidance:
(a) generic + asymmetric + modifiers; (b) `expect.extend` + `toBeOK`.

## Why §7.11 – §7.16 next

Cluster 3 wired `errors[]` and `snapshotSuffix`; cluster 5
(`toHaveScreenshot` / `toMatchAriaSnapshot` rewrites) wants
`expect.extend` registered and the asymmetric matchers in place
because `toMatchAriaSnapshot` accepts asymmetric matchers as values.
Shipping the matcher core first keeps cluster 5 from re-doing the
plumbing.

## Read-first

1. `CLAUDE.md` — rules + lessons.
2. `PLAYWRIGHT_COMPAT.md` — §7.11 – §7.16 entries.
3. `HANDOVER.md` — Cluster 3 recap (TestInfo).
4. `/tmp/playwright/packages/playwright/src/matchers/` — Playwright's
   matcher implementations. `matchers.ts` registers each matcher;
   `expect.ts` is the entry point; `asymmetricMatchers.ts` defines
   the `expect.any`/etc. set.
5. `/tmp/playwright/packages/playwright/types/test.d.ts` — search
   `interface Expect` (the chainable side) and `interface Matchers`
   (the generic side).
6. `crates/ferridriver-test/src/expect/` — current Rust matcher
   trait. `mod.rs` defines `MatchError`, `Matcher`, `MatcherResult`;
   `page.rs` and `locator.rs` are matcher impls.
7. `packages/ferridriver-test/src/expect.ts` — TS facade. Today only
   exposes `toHaveTitle`, `toHaveURL`, `toBeVisible`, etc.

## Cluster scope

### §7.11 — generic Jest matchers

Add to `crates/ferridriver-test/src/expect/`:

- `toBe`, `toEqual` (deep equality via `serde_json::Value` comparison
  for JS values).
- `toBeCloseTo`, `toBeDefined`, `toBeFalsy`, `toBeGreaterThan`,
  `toBeGreaterThanOrEqual`, `toBeInstanceOf`, `toBeLessThan`,
  `toBeLessThanOrEqual`, `toBeNaN`, `toBeNull`, `toBeTruthy`,
  `toBeUndefined`.
- `toContain`, `toContainEqual`, `toHaveLength`, `toHaveProperty`,
  `toMatch`, `toMatchObject`, `toStrictEqual`, `toThrow`,
  `toThrowError`.

Each matcher's matching logic lives in Rust (`Matcher` trait) so
the TS facade is a thin call into NAPI. The TS surface is generic
over arbitrary values, so the entry point is something like
`expect(actual: any)` returning a `ValueAssertions` chain.

### §7.12 — asymmetric matchers

`expect.any(Constructor)`, `expect.anything()`,
`expect.arrayContaining(arr)`, `expect.closeTo(num, decimals)`,
`expect.objectContaining(obj)`, `expect.stringContaining(substring)`,
`expect.stringMatching(regex)`. These don't run an assertion on
their own — they encode a partial-match predicate that other
matchers (like `toEqual`, `toMatchObject`) recognize.

Implementation: a serde-tagged struct (`{ "$asym": "objectContaining", "value": ... }`)
that the deep-equality engine in §7.11 detects and dispatches to
the right predicate.

### §7.13 — `.resolves` / `.rejects`

Promise-unwrapping modifiers. JS-side wrappers that `await` the
subject before delegating to the underlying matcher.

### §7.14 — `.soft` / `.poll` exposure

Rust core already supports `add_soft_error`. TS facade needs:

- `expect.soft(...)` returning a chain whose matchers push to
  `testInfo.soft_errors` instead of throwing. (§7.10 errors[] now
  reads them.)
- `expect.poll(probeFn, options?)` for retry-until-true polling.

### §7.15 — `expect.extend`

Register custom TS matchers into the NAPI registry. Approach:

- TS `expect.extend({ name: factoryFn })` stores the factory in a
  TS-side map and registers the name in the NAPI registry.
- When `expect(value).custom(...)` runs, the NAPI router dispatches
  by name to either the built-in Rust matcher or the TS factory.

The TS factory runs in JS (returning `{ pass, message }`) and the
result flows back to the chain's pass/fail logic.

### §7.16 — APIResponse `toBeOK`

`expect(response).toBeOK()` — pass if `response.status()` is in the
2xx range. APIResponse already exists; just add the matcher.

## Tests (Rule 9)

Per matcher group, a Bun test that:

- Creates a value, runs the matcher, asserts pass.
- Negates and asserts fail with the right error message.
- For asymmetric matchers, embed inside `toEqual` / `toMatchObject`
  and assert the deep-equality engine matches.
- For `.soft`, push 2 soft failures, assert `testInfo.errors` has 2
  entries and the test still passes.
- For `.poll`, return `false` 3 times then `true`; assert the chain
  resolves on the 4th attempt.
- For `expect.extend`, register a custom matcher and assert it
  participates in the chain.
- For `toBeOK`, hit a known 200 endpoint (httpbin or similar) and
  assert pass; hit a 500 and assert fail.

These don't need a per-backend matrix — matcher logic is
backend-agnostic.

## Ground rules (CLAUDE.md)

- Rule 1: Rust core defines the matcher logic. TS is a thin
  delegator.
- Rule 2: signature shapes match Playwright/Jest verbatim
  (`expect(actual).toBe(expected)`, not `expectEq`).
- Rule 7: rebuild NAPI and diff `index.d.ts` against
  `/tmp/playwright/packages/playwright/types/test.d.ts` (especially
  the `Matchers` and `Expect` interfaces).
- Rule 9: every matcher gets a positive + negative test.

## Baseline (must stay green)

```
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p ferridriver --lib                                 # 125
cargo test -p ferridriver-test --lib                            # 11
cargo test -p ferridriver-script --lib                          # 13
cargo test -p ferridriver-mcp --lib                             # 38
cargo test -p ferridriver-test --test new_features_e2e          # 15
cd crates/ferridriver-node && bun run build:debug && bun test   # 898
FERRIDRIVER_BIN=$(pwd)/target/debug/ferridriver \
  cargo test -p ferridriver-cli --test backends -- --test-threads=1
# cdp-pipe 175 / cdp-raw 175 / bidi 170 / webkit 171
```

## Prompt for the next session

> Continue ferridriver Playwright parity — Tier 7 cluster 4 (generic
> + asymmetric matchers, `.resolves` / `.rejects`, `.soft` / `.poll`,
> `expect.extend`, `toBeOK`, §7.11 – §7.16). Read first, in order:
>
> 1. `CLAUDE.md` — rules + lessons.
> 2. `PLAYWRIGHT_COMPAT.md` — §7.11 – §7.16.
> 3. `HANDOVER.md` — Cluster 3 recap.
> 4. `docs/NEXT_SESSION.md` — this file.
> 5. `/tmp/playwright/packages/playwright/src/matchers/` and
>    `/tmp/playwright/packages/playwright/types/test.d.ts::Matchers`.
> 6. `crates/ferridriver-test/src/expect/` and
>    `packages/ferridriver-test/src/expect.ts`.
>
> Task: ship the generic Jest matchers, asymmetric-matcher namespace,
> `.resolves` / `.rejects` modifiers, TS-side `.soft` / `.poll`,
> `expect.extend`, and `APIResponse.toBeOK`. Two commits acceptable:
> (a) generics + asymmetrics + modifiers; (b) extend + toBeOK.
>
> Rule 9 per matcher group (positive + negative).
>
> Baseline that must stay green is in HANDOVER.md.
>
> Non-negotiables: matcher logic in Rust core; TS delegates; signature
> shapes match Playwright/Jest verbatim; rebuild NAPI and diff
> index.d.ts; no shortcuts; no emojis; no AI attribution.
