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
| version | 1.65.0-next |
| commit | `d9f2fd3` |
| synced | 2026-10-09 |

## Re-sync recipe

```bash
# 1. refresh the clone (the compat harness keeps one at /tmp/playwright)
git -C /tmp/playwright fetch --all && git -C /tmp/playwright checkout <rev>

# 2. copy the vendored files
cd crates/ferridriver/src/injected
for f in domUtils highlight injectedScript layoutSelectorUtils roleSelectorEngine \
         roleUtils selectorEngine selectorEvaluator selectorGenerator selectorUtils \
         utilityScript webSocketMock xpathSelectorEngine ariaSnapshot \
         ariaSnapshotDistiller clock consoleApi webAuthn storageScript; do
  cp /tmp/playwright/packages/injected/src/$f.ts $f.ts
done
for f in ariaSnapshot ariaSnapshotRenderer cssParser cssTokenizer istanbulCoverage \
         locatorGenerators locatorParser locatorUtils selectorParser stringUtils \
         utilityScriptSerializers; do
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

`packages/injected/src/` → here: `ariaSnapshot.ts`,
`ariaSnapshotDistiller.ts`, `clock.ts`, `consoleApi.ts`, `domUtils.ts`,
`highlight.ts`, `highlight.css`, `injectedScript.ts`,
`layoutSelectorUtils.ts`, `roleSelectorEngine.ts`, `roleUtils.ts`,
`selectorEngine.ts`, `selectorEvaluator.ts`, `selectorGenerator.ts`,
`selectorUtils.ts`, `storageScript.ts`, `utilityScript.ts`, `webAuthn.ts`,
`webSocketMock.ts`, `xpathSelectorEngine.ts`.

`packages/isomorphic/` → `isomorphic/`: `ariaSnapshot.ts`,
`ariaSnapshotRenderer.ts`, `cssParser.ts`, `cssTokenizer.ts`,
`istanbulCoverage.ts`, `locatorGenerators.ts`, `locatorParser.ts`,
`locatorUtils.ts`, `selectorParser.ts`, `stringUtils.ts`,
`utilityScriptSerializers.ts`.

## ferridriver's own

Entry points and support bundles, all built by `build.ts`: `index.ts`
(the `window.__fd` surface the Rust side calls), `ariaSupport.ts`,
`axSupport.ts`, `mcpSupport.ts`, `recorderSupport.ts`, `clockEntry.ts`,
`webAuthnEntry.ts`, `webSocketMockEntry.ts`, `storageEntry.ts`, `snapshotter_injected.js`, `isomorphic/yaml.ts`,
`stubs/`.

`local/` holds code that exists only because a vendored file needs
something upstream does not have. It is empty right now: the aria
equality helpers that lived there, and the multi-attribute test-id
engine, both went away when `injectedScript.ts` / `ariaSnapshot.ts`
were brought up to 1.63 (upstream's own `_createTestIdEngine` splits
the comma form, and the tree-diffing snapshot they served was reachable
from nothing).

`stubs/structs.ts` stands in for `@protocol/structs`, the only
`@protocol` module a vendored file still imports (`injectedScript.ts`
takes `ExpectedTextValue`, `Point` and `Rect` from it).

## Deltas carried inside vendored files

None. These API changes landed in ferridriver's own files instead when
upstream made them:

- 1.65 threads a `frameVisible` argument through `elementState` and
  `checkElementStates`: an element inside a hidden iframe is hidden.
  Playwright computes it host-side while resolving frame hops;
  `index.ts::frameVisible` walks the element's `frameElement` chain,
  which covers same-origin frames and takes a cross-origin ancestor as
  visible.
- 1.65 replaced `addHighlight`/`removeHighlight` with a declarative
  `setHighlights([...])`. `index.ts` exposes it as `__fd.setHighlights`,
  and `Page`'s highlight controller re-applies each frame's set the way
  `server/highlightController.ts` does. `recorderSupport.ts` shims the
  same method onto its own `Highlight`.

- `getElementAccessibleName` returns a composite `AccessibleName` since
  1.63; callers that want the string use `getElementAccessibleNameText`,
  and `getElementAccessibleDescription(...).text` likewise. `index.ts`
  exposes the text form as `__fd.getAccessibleName`, because the Rust
  side reads a string.
- `InjectedScriptOptions` gained `frameSeq`, which the renderer turns
  into the `f<seq>e<n>` ref prefix. Playwright builds its injected
  options per frame; ferridriver injects one script per page, so
  `index.ts` passes `0` and the host prefixes a child frame's refs when
  it splices the frame's render (`locator.rs::aria_stitch_frame`).
- Snapshot rendering split into `renderAriaTreeAsJSON` (tree → JSON) and
  `renderAriaSnapshotAsYaml` (JSON → YAML). `__fd.ariaSnapshotFrame`
  runs both, because the Rust side stitches iframes as text.
