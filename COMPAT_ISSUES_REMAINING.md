# ferridriver compat issues — verification after the parity/drag-robustness pass

Re-tested on the rebuilt binary (ferridriver 0.5.0, commit 5ec9419) against the
real app.acme.com Sign flow and minimal local pages.

## FIXED (verified)
- **Locator.all()** — returns `Locator[]`, iterable. ✓
- **`run -c <config>` extensions** — the config `extensions:` list now loads in
  the `run` host (`box` global + all 15 tools). ✓
- **A. `dragTo` single-jump** — `steps` now defaults to 5 interpolated
  `mousemove`s (Playwright defaults to 1 but compensates with native drag
  interception); additionally the full `crDragDrop.ts` DragManager was ported
  to the CDP backend (`Input.setInterceptDrags` + `Input.dispatchDragEvent`),
  so native HTML5 DnD works end-to-end on `cdp-pipe`/`cdp-raw` with a real
  `DataTransfer` payload, and Escape cancels an in-flight drag. Verified on
  all 4 backends: `test_script_drag_default_steps`,
  `test_script_drag_native_html5` (`tests/backends_support/script_handles_local.rs`).
  Known hole: Firefox over BiDi starts the drag but never delivers the final
  `drop` — same as Playwright's own BiDi backend (tracked in
  `docs/PLAYWRIGHT-PARITY-BACKLOG.md`).
- **B. `setInputFiles` stale objectId on hidden React-managed input** — the
  selector-string path (main-frame `document.querySelector` + captured
  objectId) was replaced by the locator retry funnel: the element resolves
  through the selector engine in its owning frame immediately before the
  protocol call, and a stale node re-resolves instead of surfacing
  `Object id doesn't reference a Node`. Also fixed on the way: iframe-scoped
  inputs (previously main-frame only), engine selectors (`getByTestId` etc.),
  `Uint8Array`/`ArrayBuffer` payload buffers in the QuickJS host, and WebKit
  now pairs `DOM.setInputFiles` with `Playwright.grantFileReadAccess` like
  Playwright. Verified on all 4 backends:
  `test_script_set_input_files_hidden_remounting`,
  `test_script_set_input_files_in_iframe`,
  `test_script_set_input_files_engine_selector_payload`
  (`tests/backends_support/script_locators.rs`).
- **C. Auto-wait / stale retry coverage** — both action paths above now run
  under the standard retry funnel. ✓

## STILL FAILING

(nothing verified-failing at the moment — re-test the real app.acme.com Sign
flow on the next rebuilt binary and log regressions here)

New script-sandbox parity gaps found on the 2026-07-16 signer-flow run
(`context.waitForEvent`, `context.pages()`, `filter({hasText: RegExp})`,
`waitForURL` timeout option) are tracked in
`docs/PLAYWRIGHT-PARITY-BACKLOG.md` under "Script-sandbox gaps hit driving
the real app.acme.com Sign flow".

## Live app verification (real app.acme.com Sign, cdp-pipe, via box plugin)

Re-tested end-to-end after the fixes, not just synthetic pages:

- **A. dragTo** — `panelPlaceholder.dragTo(canvas, { targetPosition })` places the
  field on the real doc-prep canvas (`[data-placeholder-id]` 0 -> 1). ✓
- **B. setInputFiles** — `[data-test-id="file-upload-desktop"] input[type=file]`
  (hidden, React-remounting) `.setInputFiles(pdf)` uploads and the canvas
  renders. ✓
- Full `box.sign.createRequest` (enter builder -> upload -> add recipient ->
  place + assign signature field) runs green.

The box plugin's two workarounds (file-chooser upload, manual stepped mouse
drag) have been REMOVED and reverted to idiomatic `setInputFiles` / `dragTo`,
matching the sign-client Playwright suite.
