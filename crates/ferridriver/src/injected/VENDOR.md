# Vendored from Playwright

The selector engine, actionability checks, role/aria computation and the
clock live here as **verbatim copies of Playwright's sources**. They are
not a fork: a file listed as vendored below must stay byte-identical to
its upstream counterpart, so a re-sync is `cp`, never a merge.

Anything ferridriver adds or has to keep alive after upstream deletes it
goes in `local/`, imported by name. If you find yourself editing a
vendored file, put the change in `local/` instead — or, when the change
belongs upstream, send it there.

## Current revision

| | |
|---|---|
| upstream | `microsoft/playwright` |
| version | 1.63.0-next |
| commit | `07730b7` |
| synced | 2026-08-17 |

## Re-sync recipe

```bash
# 1. refresh the clone (the compat harness keeps one at /tmp/playwright)
git -C /tmp/playwright fetch --all && git -C /tmp/playwright checkout <rev>

# 2. copy the vendored files
cd crates/ferridriver/src/injected
for f in domUtils highlight injectedScript layoutSelectorUtils roleSelectorEngine \
         roleUtils selectorEngine selectorEvaluator selectorGenerator selectorUtils \
         utilityScript webSocketMock xpathSelectorEngine ariaSnapshot clock consoleApi; do
  cp /tmp/playwright/packages/injected/src/$f.ts $f.ts
done
for f in ariaSnapshot cssParser cssTokenizer locatorGenerators locatorParser \
         locatorUtils selectorParser stringUtils utilityScriptSerializers; do
  cp /tmp/playwright/packages/isomorphic/$f.ts isomorphic/$f.ts
done
cp /tmp/playwright/packages/injected/src/highlight.css highlight.css

# 3. type-check against the baseline, rebuild, then run the browser suite
bun x tsc --noEmit -p tsconfig.json
bun build.ts
./target/debug/ferridriver test tests/e2e/locators.test.ts tests/e2e/handles.test.ts \
                                tests/e2e/actions.test.ts tests/e2e/expect.test.ts
```

`build.ts` emits every bundle under `dist/`; those are build output, and
the Rust side `include_str!`s them.

## Vendored files

`packages/injected/src/` → here: `ariaSnapshot.ts`, `clock.ts`,
`consoleApi.ts`, `domUtils.ts`, `highlight.ts`, `highlight.css`,
`injectedScript.ts`, `layoutSelectorUtils.ts`, `roleSelectorEngine.ts`,
`roleUtils.ts`, `selectorEngine.ts`, `selectorEvaluator.ts`,
`selectorGenerator.ts`, `selectorUtils.ts`, `utilityScript.ts`,
`webSocketMock.ts`, `xpathSelectorEngine.ts`.

`packages/isomorphic/` → `isomorphic/`: `ariaSnapshot.ts`,
`cssParser.ts`, `cssTokenizer.ts`, `locatorGenerators.ts`,
`locatorParser.ts`, `locatorUtils.ts`, `selectorParser.ts`,
`stringUtils.ts`, `utilityScriptSerializers.ts`.

## ferridriver's own

Entry points and support bundles, all built by `build.ts`: `index.ts`
(the `window.__fd` surface the Rust side calls), `ariaSupport.ts`,
`axSupport.ts`, `mcpSupport.ts`, `recorderSupport.ts`, `clockEntry.ts`,
`webSocketMockEntry.ts`, `snapshotter_injected.js`, `isomorphic/yaml.ts`,
`stubs/`.

`local/` holds code that exists only because a vendored file needs
something upstream does not have:

| module | why |
|---|---|
| `local/ariaEquality.ts` | `ariaNodesEqual` / `ariaPropsEqual`, deleted upstream when snapshot rendering split into `renderAriaTreeAsJSON` + `ariaSnapshotRenderer`. ferridriver's incremental snapshot (`__fd.incrementalAriaSnapshot`) diffs two trees node by node and still needs them. |

## Deltas carried inside vendored files

None. Two API changes landed in ferridriver's own files instead when
upstream made them:

- `getElementAccessibleName` returns a composite `AccessibleName` since
  1.63; callers that want the string use `getElementAccessibleNameText`,
  and `getElementAccessibleDescription(...).text` likewise. `index.ts`
  exposes the text form as `__fd.getAccessibleName`, because the Rust
  side reads a string.
- `InjectedScriptOptions` gained `frameSeq`; `index.ts` passes it.
