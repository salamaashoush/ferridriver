#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::single_char_pattern,
  clippy::unwrap_used,
  clippy::expect_used
)]
//! run_script: Locator chains / waits / upload tests, extracted from backends.rs.

use serde_json::json;

use super::client::McpClient;

// Task 3.8: Playwright-parity sync frame accessors exposed via QuickJS.
// Verifies the same FrameJs surface the NAPI tests cover — name/url/
// isMainFrame/parentFrame/childFrames/isDetached are all sync (no await).
pub fn test_script_frame_sync_accessors(c: &mut McpClient) {
  c.nav(
    "<h1>Parent</h1>\
     <iframe name='alpha' srcdoc='<p>A</p>'></iframe>\
     <iframe name='beta' srcdoc='<p>B</p>'></iframe>",
  );
  // Wait for both iframes to appear in the DOM — by the time
  // waitForSelector resolves, FrameAttached/Navigated events have
  // propagated to the page-owned frame cache.
  // Use `== null` (loose equality) to accept both rquickjs `undefined` and
  // explicit `null` — rquickjs maps `Option::None` returns to JS
  // `undefined`, not `null`.
  let v = c.script_value(
    "await page.waitForSelector('iframe[name=\"alpha\"]'); \
       await page.waitForSelector('iframe[name=\"beta\"]'); \
       const main = page.mainFrame(); \
       const kidNames = main.childFrames().map(f => f.name()).sort(); \
       const alpha = page.frame('alpha'); \
       const alphaParent = alpha ? alpha.parentFrame() : null; \
       return { \
         mainIsMain: main.isMainFrame(), \
         mainParentNull: main.parentFrame() == null, \
         mainDetached: main.isDetached(), \
         kidNames, \
         alphaName: alpha ? alpha.name() : null, \
         alphaIsMain: alpha ? alpha.isMainFrame() : null, \
         alphaParentIsMain: alphaParent ? alphaParent.isMainFrame() : null, \
         frameCount: page.frames().length, \
       };",
  );
  assert_eq!(v["mainIsMain"], json!(true), "mainFrame.isMainFrame(): {v}");
  assert_eq!(
    v["mainParentNull"],
    json!(true),
    "mainFrame.parentFrame() === null: {v}"
  );
  assert_eq!(v["mainDetached"], json!(false), "mainFrame.isDetached() === false: {v}");
  assert_eq!(v["alphaName"], json!("alpha"), "frame('alpha').name(): {v}");
  assert_eq!(v["alphaIsMain"], json!(false), "child frame is not main: {v}");
  assert_eq!(v["alphaParentIsMain"], json!(true), "child.parentFrame() is main: {v}");
  assert!(
    v["frameCount"].as_i64().unwrap_or(0) >= 3,
    "frames() includes main + 2 iframes: {v}"
  );
  let kids = v["kidNames"].as_array().cloned().unwrap_or_default();
  assert!(
    kids.iter().any(|n| n == &json!("alpha")),
    "child names contain 'alpha': {v}"
  );
  assert!(
    kids.iter().any(|n| n == &json!("beta")),
    "child names contain 'beta': {v}"
  );
}

pub fn test_script_frame_selector_union(c: &mut McpClient) {
  c.nav("<iframe name='target' src='about:blank'></iframe>");
  let v = c.script_value(
    "await page.waitForSelector('iframe[name=\"target\"]'); \
       const byName = page.frame('target'); \
       const byObj = page.frame({ name: 'target' }); \
       const empty = page.frame({}); \
       return { \
         byNameName: byName ? byName.name() : null, \
         byObjName: byObj ? byObj.name() : null, \
         emptyIsNull: empty == null, \
       };",
  );
  assert_eq!(v["byNameName"], json!("target"), "frame(string) resolves: {v}");
  assert_eq!(v["byObjName"], json!("target"), "frame({{name}}) resolves: {v}");
  assert_eq!(v["emptyIsNull"], json!(true), "frame({{}}) returns null: {v}");
}

pub fn test_script_wait_for_selector(c: &mut McpClient) {
  c.nav("<div id='target'>here</div>");
  let v = c.script_value("await page.waitForSelector('#target'); return 'ok';");
  assert_eq!(v, json!("ok"));
}

// Fix #14: Frame.waitForSelector returns the matched ElementHandle for
// `state: 'attached' | 'visible'` (default) and null for
// `hidden` / `detached`, mirroring
// /tmp/playwright/packages/playwright-core/src/client/frame.ts:217.
pub fn test_script_frame_wait_for_selector_handle(c: &mut McpClient) {
  c.nav("<div id='t'>payload-text</div><div id='hid' style='display:none'>x</div>");
  let v = c.script_value(
    "const main = page.mainFrame(); \
       const h = await main.waitForSelector('#t'); \
       const hidden = await main.waitForSelector('#hid', { state: 'hidden' }); \
       return { \
         handleText: h ? await h.textContent() : null, \
         handleIsNull: h == null, \
         hiddenIsNull: hidden == null, \
       };",
  );
  // Observable effect: the returned object is the resolved element, so
  // reading its text yields the element content — only possible when the
  // handle is the real match rather than the old `()` return.
  assert_eq!(v["handleText"], json!("payload-text"), "returned handle text: {v}");
  assert_eq!(v["handleIsNull"], json!(false), "default state returns a handle: {v}");
  assert_eq!(v["hiddenIsNull"], json!(true), "state:'hidden' returns null: {v}");
}

// Fix #14: waitForSelector resolves inside a child frame and returns that
// frame's element (not the parent's).
pub fn test_script_frame_wait_for_selector_in_child(c: &mut McpClient) {
  c.nav("<iframe name='child' srcdoc=\"<div id='inner'>inner-payload</div>\"></iframe>");
  let v = c.script_value(
    "await page.waitForSelector('iframe[name=\"child\"]'); \
       const frame = page.frame('child'); \
       const h = await frame.waitForSelector('#inner'); \
       return h ? await h.textContent() : null;",
  );
  assert_eq!(v, json!("inner-payload"), "child-frame handle text: {v}");
}

pub fn test_script_wait_for_text(c: &mut McpClient) {
  c.nav("<body></body><script>setTimeout(function(){document.body.innerHTML='<p>findme</p>'}, 100)</script>");
  let v = c.script_value(
    "await page.waitForSelector('p'); \
       return await page.textContent('p');",
  );
  assert_eq!(v, json!("findme"));
}

pub fn test_script_auto_wait_visibility(c: &mut McpClient) {
  c.nav("<button style='display:none' id='b' onclick=\"this.textContent='ok'\">Go</button><script>setTimeout(function(){document.getElementById('b').style.display=''},500)</script>");
  let v = c.script_value("await page.click('#b'); return await page.textContent('#b');");
  assert_eq!(v, json!("ok"), "click should auto-wait for visible");
}

pub fn test_script_locator_role(c: &mut McpClient) {
  c.nav("<button>Save</button><button disabled>Delete</button>");
  let v = c.script_value(
    "await page.getByRole('button').first().click(); \
       return await page.getByRole('button').count();",
  );
  assert_eq!(v, json!(2), "getByRole should find 2 buttons");
}

pub fn test_script_locator_label(c: &mut McpClient) {
  c.nav("<label for='e'>Email Address</label><input id='e' type='email'>");
  let v = c.script_value(
    "await page.getByLabel('Email Address').fill('test@test.com'); \
       return await page.inputValue('#e');",
  );
  assert_eq!(v, json!("test@test.com"));
}

pub fn test_script_locator_placeholder(c: &mut McpClient) {
  c.nav("<input placeholder='Enter your name' id='n'>");
  let v = c.script_value(
    "await page.getByPlaceholder('Enter your name').fill('Alice'); \
       return await page.inputValue('#n');",
  );
  assert_eq!(v, json!("Alice"));
}

pub fn test_script_locator_text(c: &mut McpClient) {
  c.nav("<button>First</button><button>Second</button><button>Third</button>");
  let v = c.script_value("return await page.getByText('Second').textContent();");
  assert_eq!(v, json!("Second"));
}

pub fn test_script_locator_nth(c: &mut McpClient) {
  c.nav("<button>alpha</button><button>beta</button><button>gamma</button>");
  let v = c.script_value("return await page.getByRole('button').nth(1).textContent();");
  assert_eq!(v, json!("beta"));
}

pub fn test_script_locator_all_text(c: &mut McpClient) {
  c.nav("<li>a</li><li>b</li><li>c</li>");
  let v = c.script_value("return await page.locator('li').allTextContents();");
  assert_eq!(v, json!(["a", "b", "c"]));
}

pub fn test_script_selector_chain(c: &mut McpClient) {
  c.nav("<div class='a'><button onclick=\"this.textContent='clicked'\">Yes</button></div><div class='b'><button>No</button></div>");
  let v = c.script_value(
    "await page.locator('.a').locator('button').click(); \
       return await page.locator('.a button').textContent();",
  );
  assert_eq!(v, json!("clicked"), "chained locator should click button in .a");
}

pub fn test_script_upload_file(c: &mut McpClient) {
  c.nav("<input type='file' id='f'><div id='r'></div><script>document.getElementById('f').addEventListener('change',function(e){var f=e.target.files[0];if(f){var reader=new FileReader();reader.onload=function(){document.getElementById('r').textContent='name:'+f.name+',size:'+f.size+',content:'+reader.result;};reader.readAsText(f);}});</script>");
  let tmp = std::env::temp_dir().join("ferridriver_test_upload.txt");
  std::fs::write(&tmp, "test file content").unwrap();
  let v = c.script_value_with_args(
    "await page.setInputFiles('#f', [args[0]]); \
       const count = await page.evaluate(\"document.getElementById('f').files.length\"); \
       const name = await page.evaluate(\"document.getElementById('f').files[0].name\"); \
       const size = await page.evaluate(\"document.getElementById('f').files[0].size\"); \
       return { count: count, name: name, size: size };",
    json!([tmp.to_str().unwrap()]),
  );
  assert_eq!(v["count"], json!(1));
  assert_eq!(v["name"], json!("ferridriver_test_upload.txt"));
  assert_eq!(v["size"], json!(17));
  let _ = std::fs::remove_file(&tmp);
}

// Playwright: `locator.normalize(): Promise<Locator>`
// (client/locator.ts:269 -> server frames.ts:1274 resolveSelector ->
// injected.generateSelectorSimple). normalize() must return a NEW
// locator whose selector is the canonical recorder form for the matched
// element. Observable effect: the input is a text selector but the
// normalized selector is the generated `internal:testid` / id form
// (clearly different from the input) AND still resolves to the same
// single element, so an action through it hits the same node.
pub fn test_script_locator_normalize(c: &mut McpClient) {
  c.nav(
    "<button data-testid='save-btn' onclick=\"this.dataset.hit='1'\">Save</button>\
     <button>Cancel</button>",
  );
  let v = c.script_value(
    "const orig = page.getByText('Save'); \
       const norm = await orig.normalize(); \
       const normSel = norm.selector; \
       const origSel = orig.selector; \
       await norm.click(); \
       const count = await norm.count(); \
       const hit = await page.evaluate(\"document.querySelector('[data-testid=save-btn]').dataset.hit\"); \
       return { origSel, normSel, count, hit, changed: normSel !== origSel };",
  );
  assert_eq!(
    v["count"],
    json!(1),
    "normalized locator resolves to exactly one element: {v}"
  );
  assert_eq!(v["changed"], json!(true), "normalized selector differs from input: {v}");
  assert_eq!(
    v["hit"].as_str(),
    Some("1"),
    "click through normalized locator hit the same Save button: {v}"
  );
  // generateSelectorSimple prefers the data-testid attribute for an
  // element that has one — proves the canonical recorder form, not a
  // pass-through of the original text selector.
  let norm_sel = v["normSel"].as_str().unwrap_or_default();
  assert!(
    norm_sel.contains("save-btn"),
    "normalized selector uses the canonical testid form: {v}"
  );
}

// Locator.highlight installs the Playwright glass-pane overlay
// (`<x-pw-glass>`) on documentElement; hideHighlight / the returned
// Disposable's dispose() tears it down. The overlay element only exists
// when addHighlight actually ran, so its presence/absence is a real
// effect of the call, not just a non-error.
// Playwright: client/locator.ts:158 (highlight) + :164 (hideHighlight).
pub fn test_script_locator_highlight(c: &mut McpClient) {
  c.nav("<button id='b'>Target</button>");
  let v = c.script_value(
    "const loc = page.locator('#b'); \
       const before = await page.evaluate(\"document.querySelectorAll('x-pw-glass').length\"); \
       const disp = await loc.highlight({ style: { outlineColor: 'red', zIndex: 7 } }); \
       const during = await page.evaluate(\"document.querySelectorAll('x-pw-glass').length\"); \
       await disp.dispose(); \
       const afterDispose = await page.evaluate(\"document.querySelectorAll('x-pw-glass').length\"); \
       await loc.highlight(); \
       const reAdded = await page.evaluate(\"document.querySelectorAll('x-pw-glass').length\"); \
       await loc.hideHighlight(); \
       const afterHide = await page.evaluate(\"document.querySelectorAll('x-pw-glass').length\"); \
       return { \
         before: Number(before), \
         during: Number(during), \
         afterDispose: Number(afterDispose), \
         reAdded: Number(reAdded), \
         afterHide: Number(afterHide), \
       };",
  );
  assert_eq!(v["before"], json!(0), "no overlay before highlight: {v}");
  assert_eq!(v["during"], json!(1), "overlay installed by highlight(): {v}");
  assert_eq!(v["afterDispose"], json!(0), "Disposable.dispose() removes overlay: {v}");
  assert_eq!(
    v["reAdded"],
    json!(1),
    "highlight() without style re-installs overlay: {v}"
  );
  assert_eq!(v["afterHide"], json!(0), "hideHighlight() removes overlay: {v}");
}

// QuickJS<->NAPI parity: Locator.selector / isStrict / setStrict /
// selectText / rightClick / boundingBox were NAPI-only until added to the
// script binding. Exercise each so the binding surface stays in lockstep.
pub fn test_script_locator_napi_parity(c: &mut McpClient) {
  c.nav(
    "<button id='b' oncontextmenu=\"this.dataset.rc='1';return false\">Target</button>\
     <input id='inp' value='select me'>",
  );
  let v = c.script_value(
    "const b = page.locator('#b'); \
       const sel = b.selector; \
       const strictDefault = b.isStrict; \
       const loose = b.setStrict(false); \
       const looseStrict = loose.isStrict; \
       const box = await b.boundingBox(); \
       await b.rightClick(); \
       const rc = await page.evaluate(\"document.getElementById('b').dataset.rc\"); \
       await page.locator('#inp').selectText(); \
       const selText = await page.evaluate(\"String(window.getSelection ? document.getSelection().toString() : '') || (document.activeElement && document.activeElement.id)\"); \
       return { sel, strictDefault, looseStrict, \
         hasBox: box != null && box.width > 0 && box.height > 0, \
         rc, selText };",
  );
  assert_eq!(v["sel"], json!("#b"), "selector getter returns the selector: {v}");
  assert_eq!(v["strictDefault"], json!(true), "locators are strict by default: {v}");
  assert_eq!(v["looseStrict"], json!(false), "setStrict(false) clears strict: {v}");
  assert_eq!(v["hasBox"], json!(true), "boundingBox returns a non-empty rect: {v}");
  assert_eq!(v["rc"].as_str(), Some("1"), "rightClick fired contextmenu: {v}");
  // selectText focuses+selects the input; either the selection text or the
  // focused element id confirms it ran (core uses input.select()).
  let st = v["selText"].as_str().unwrap_or_default();
  assert!(
    st.contains("select me") || st == "inp",
    "selectText selected/focused the input: {v}"
  );
}

// frameLocator enter-frame hops must resolve through the READ / WAIT
// paths, not just the action funnel. `click` always resolved (retry
// macro), but `waitFor` / `isVisible` / `isAttached` / `innerText`
// queried the raw `iframe >> internal:control=enter-frame >> #c` chain
// in the parent frame — the engine's enter-frame control returns `[]`
// by design, so they timed out / reported false. Exercises the child
// frame via srcdoc AND a data: URL (both have no name/url the frame
// cache heuristic could match — the deterministic content-frame path
// is the only thing that resolves them).
pub fn test_script_frame_locator_enter_frame_reads(c: &mut McpClient) {
  for (label, html) in [
    ("srcdoc", "<iframe srcdoc='<button id=c>child</button>'></iframe>"),
    (
      "dataurl",
      "<iframe src='data:text/html,<button id=c>child</button>'></iframe>",
    ),
  ] {
    c.nav(html);
    let v = c.script_value(
      "const inner = page.frameLocator('iframe').locator('#c'); \
       await inner.waitFor({ timeout: 10000 }); \
       return { \
         visible: await inner.isVisible(), \
         attached: await inner.isAttached(), \
         text: await inner.innerText(), \
         count: await inner.count(), \
       };",
    );
    assert_eq!(v["visible"], json!(true), "{label}: enter-frame isVisible: {v}");
    assert_eq!(v["attached"], json!(true), "{label}: enter-frame isAttached: {v}");
    assert_eq!(v["text"], json!("child"), "{label}: enter-frame innerText: {v}");
    assert_eq!(v["count"], json!(1), "{label}: enter-frame count: {v}");
  }
}

// Playwright: `locator.all()` returns one Locator per matching element
// (nth=0..count-1). Proves each returned handle resolves its OWN element,
// not just that the call didn't throw.
pub fn test_script_locator_all(c: &mut McpClient) {
  c.nav("<ul><li>one</li><li>two</li><li>three</li></ul>");
  let v = c.script_value(
    "const items = await page.locator('li').all(); \
     const texts = []; \
     for (const it of items) texts.push((await it.textContent()).trim()); \
     return { count: items.length, texts };",
  );
  assert_eq!(v["count"], json!(3), "all() yields one locator per element: {v}");
  assert_eq!(
    v["texts"],
    json!(["one", "two", "three"]),
    "each locator resolves its own element in order: {v}"
  );
}

// Playwright: `locator.waitForFunction(fn, arg?, options?)` polls the
// element-scoped predicate until truthy. A page-side setTimeout flips the
// text after the first poll, so a passing test proves the loop actually
// re-polled (not that the predicate was already true).
pub fn test_script_locator_wait_for_function(c: &mut McpClient) {
  c.nav("<div id='t'>pending</div>");
  let v = c.script_value(
    "const el = page.locator('#t'); \
     await page.evaluate(() => { setTimeout(() => { document.getElementById('t').textContent = 'ready'; }, 60); }); \
     await el.waitForFunction(node => node.textContent === 'ready'); \
     return { text: (await el.textContent()).trim() };",
  );
  assert_eq!(
    v["text"],
    json!("ready"),
    "waitForFunction polled the element until the predicate turned truthy: {v}"
  );
}

pub fn register(set: &mut crate::TestSet<'_>) {
  set.run(
    "backends_support::script_locators::test_script_locator_all",
    test_script_locator_all,
  );
  set.run(
    "backends_support::script_locators::test_script_locator_wait_for_function",
    test_script_locator_wait_for_function,
  );
  set.run(
    "backends_support::script_locators::test_script_frame_locator_enter_frame_reads",
    test_script_frame_locator_enter_frame_reads,
  );
  set.run(
    "backends_support::script_locators::test_script_frame_sync_accessors",
    test_script_frame_sync_accessors,
  );
  set.run(
    "backends_support::script_locators::test_script_frame_selector_union",
    test_script_frame_selector_union,
  );
  set.run(
    "backends_support::script_locators::test_script_wait_for_selector",
    test_script_wait_for_selector,
  );
  set.run(
    "backends_support::script_locators::test_script_frame_wait_for_selector_handle",
    test_script_frame_wait_for_selector_handle,
  );
  set.run(
    "backends_support::script_locators::test_script_frame_wait_for_selector_in_child",
    test_script_frame_wait_for_selector_in_child,
  );
  set.run(
    "backends_support::script_locators::test_script_wait_for_text",
    test_script_wait_for_text,
  );
  set.run(
    "backends_support::script_locators::test_script_auto_wait_visibility",
    test_script_auto_wait_visibility,
  );
  set.run(
    "backends_support::script_locators::test_script_locator_role",
    test_script_locator_role,
  );
  set.run(
    "backends_support::script_locators::test_script_locator_label",
    test_script_locator_label,
  );
  set.run(
    "backends_support::script_locators::test_script_locator_placeholder",
    test_script_locator_placeholder,
  );
  set.run(
    "backends_support::script_locators::test_script_locator_text",
    test_script_locator_text,
  );
  set.run(
    "backends_support::script_locators::test_script_locator_nth",
    test_script_locator_nth,
  );
  set.run(
    "backends_support::script_locators::test_script_locator_all_text",
    test_script_locator_all_text,
  );
  set.run(
    "backends_support::script_locators::test_script_selector_chain",
    test_script_selector_chain,
  );
  set.run(
    "backends_support::script_locators::test_script_upload_file",
    test_script_upload_file,
  );
  set.run(
    "backends_support::script_locators::test_script_locator_normalize",
    test_script_locator_normalize,
  );
  set.run(
    "backends_support::script_locators::test_script_locator_highlight",
    test_script_locator_highlight,
  );
  set.run(
    "backends_support::script_locators::test_script_locator_napi_parity",
    test_script_locator_napi_parity,
  );
}
