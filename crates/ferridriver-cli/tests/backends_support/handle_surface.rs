//! `JSHandle` / `ElementHandle` behaviour tests that exercise the
//! full Playwright-parity surface beyond the lifecycle basics —
//! `jsonValue` / `getProperty` / `getProperties`, multi-arg
//! `handle.evaluate(fn, userArg)`, `$eval` / `$$eval`,
//! `ownerFrame` / `contentFrame`, element-scoped `waitFor*`,
//! the temp-tag action bridge (`fill` / `check` / etc.), and
//! `selectText`.
//!
//! Every function here runs on all four backends via the runner in
//! `tests/backends.rs`. Tests that target a single backend (WebKit's
//! observable `window.__wr` shrink, for example) live alongside the
//! behaviour they probe, not here.

use super::client::McpClient;
use serde_json::json;

pub fn test_handle_json_value(c: &mut McpClient) {
  c.nav("<button id='primary'>ok</button>");

  // jsonValue round-trips JSON-expressible values through the utility
  // script's isomorphic serializer.
  let v = c.script_value(
    "const jh = await page.evaluateHandle(() => ({a: 1, b: 'two', c: [3, 4]}));\
     const v = await jh.jsonValue();\
     await jh.dispose();\
     return v;",
  );
  assert_eq!(v["a"], json!(1), "jsonValue.a: {v}");
  assert_eq!(v["b"], json!("two"), "jsonValue.b: {v}");
  assert_eq!(v["c"], json!([3, 4]), "jsonValue.c: {v}");

  // jsonValue rehydrates rich types (Date, NaN, BigInt, typed arrays)
  // into native JS — matching Playwright's `parseResult` in
  // `/tmp/playwright/packages/playwright-core/src/protocol/serializers.ts:19`.
  let v = c.script_value(
    "const jh = await page.evaluateHandle(() => ({d: new Date(0), n: NaN}));\
     const v = await jh.jsonValue();\
     await jh.dispose();\
     return {d_is_date: v.d instanceof Date, d_iso: v.d.toISOString(), n_is_nan: Number.isNaN(v.n)};",
  );
  assert_eq!(v["d_is_date"], json!(true), "jsonValue rehydrates Date: {v}");
  assert_eq!(
    v["d_iso"],
    json!("1970-01-01T00:00:00.000Z"),
    "Date preserves epoch-zero: {v}"
  );
  assert_eq!(v["n_is_nan"], json!(true), "jsonValue rehydrates NaN: {v}");
}

pub fn test_handle_properties(c: &mut McpClient) {
  c.nav("<button id='primary'>ok</button>");

  // getProperty on both primitive and object values. Playwright's
  // JSHandle can be backed by either a remote reference (`_objectId`)
  // or an inline primitive (`_value`) — the two shapes round-trip
  // through jsonValue identically.
  let v = c.script_value(
    "const jh = await page.evaluateHandle(() => ({x: 42, y: 'hi', z: {n: 7}}));\
     const xh = await jh.getProperty('x');\
     const xv = await xh.jsonValue();\
     const yh = await jh.getProperty('y');\
     const yv = await yh.jsonValue();\
     const zh = await jh.getProperty('z');\
     const zv = await zh.jsonValue();\
     await xh.dispose(); await yh.dispose(); await zh.dispose(); await jh.dispose();\
     return {x: xv, y: yv, z: zv};",
  );
  assert_eq!(v["x"], json!(42), "getProperty('x').jsonValue (primitive): {v}");
  assert_eq!(v["y"], json!("hi"), "getProperty('y').jsonValue (primitive): {v}");
  assert_eq!(v["z"], json!({"n": 7}), "getProperty('z').jsonValue (object): {v}");

  // getProperties enumerates own enumerable string-keyed props as
  // (key, JSHandle) pairs. Handles of primitive-valued props are
  // value-backed; object-valued props are remote-backed. Dispose is
  // a no-op for value-backed handles.
  let v = c.script_value(
    "const jh = await page.evaluateHandle(() => ({a: 1, b: 2}));\
     const props = await jh.getProperties();\
     const keys = Object.keys(props).sort();\
     const a = await props.a.jsonValue();\
     const b = await props.b.jsonValue();\
     await props.a.dispose(); await props.b.dispose(); await jh.dispose();\
     return {keys, a, b};",
  );
  assert_eq!(v["keys"], json!(["a", "b"]), "getProperties keys: {v}");
  assert_eq!(v["a"], json!(1), "getProperties.a.jsonValue: {v}");
  assert_eq!(v["b"], json!(2), "getProperties.b.jsonValue: {v}");
}

pub fn test_handle_multi_arg_evaluate(c: &mut McpClient) {
  c.nav("<body><button id='primary'>ok</button></body>");

  // `handle.evaluate(fn, userArg)` passes the handle AND the user arg
  // as two positional parameters — the user function signature is
  // `(target, userArg) => ...`. Mirrors Playwright's
  // `javascript.ts:161-163` `evaluate(ctx, true, fn, this, arg)`.
  let v = c.script_value(
    "const eh = await page.querySelector('button#primary');\
     const out = await eh.evaluate((el, suffix) => el.tagName + suffix, '!');\
     await eh.dispose();\
     return out;",
  );
  assert_eq!(v, json!("BUTTON!"), "multi-arg handle.evaluate: {v}");

  // Passing a JSHandle AS the user arg exercises the rich-arg walker
  // (top-level class-instance detection → `{h: 0}` wire shape).
  let v = c.script_value(
    "const body = await page.querySelector('body');\
     const btn = await page.querySelector('button#primary');\
     const out = await btn.evaluate((el, other) => other.contains(el), body);\
     await btn.dispose(); await body.dispose();\
     return out;",
  );
  assert_eq!(v, json!(true), "handle-as-user-arg rich walker: {v}");
}

pub fn test_element_handle_eval(c: &mut McpClient) {
  c.nav("<div id='parent'><button class='b'>one</button><button class='b'>two</button></div>");

  // $eval runs `fn` with the first matched descendant as arg.
  let v = c.script_value(
    "const p = await page.querySelector('#parent');\
     const out = await p.$eval('button.b', el => el.textContent);\
     await p.dispose();\
     return out;",
  );
  assert_eq!(v, json!("one"), "$eval text: {v}");

  // $$eval runs `fn` with the array of matches as arg.
  let v = c.script_value(
    "const p = await page.querySelector('#parent');\
     const out = await p.$$eval('button.b', els => els.map(e => e.textContent).join('|'));\
     await p.dispose();\
     return out;",
  );
  assert_eq!(v, json!("one|two"), "$$eval texts: {v}");

  // $eval on a missing selector errors (Playwright parity).
  let v = c.script(
    "const p = await page.querySelector('#parent');\
     try {\
       const out = await p.$eval('button.does-not-exist', el => el.textContent);\
       await p.dispose();\
       return {ok: true, out};\
     } catch (e) {\
       await p.dispose();\
       return {ok: false, msg: String(e)};\
     }",
  );
  assert_eq!(
    v["value"]["ok"],
    json!(false),
    "$eval on missing selector should error: {v:?}"
  );

  // $$eval with no match returns an empty array — not an error.
  let v = c.script_value(
    "const p = await page.querySelector('#parent');\
     const out = await p.$$eval('button.none', els => els.length);\
     await p.dispose();\
     return out;",
  );
  assert_eq!(v, json!(0), "$$eval empty match length: {v}");
}

pub fn test_element_handle_query(c: &mut McpClient) {
  c.nav("<div id='parent'><button class='b'>one</button><span class='b'>two</span><button class='b'>three</button></div><button class='b'>outside</button>");

  // $ resolves the first descendant inside this element's subtree only
  // — the `outside` button is a sibling of #parent, so it must not be
  // returned even though it matches `.b`.
  let v = c.script_value(
    "const p = await page.querySelector('#parent');\
     const first = await p.$('.b');\
     const txt = await first.textContent();\
     await first.dispose();\
     await p.dispose();\
     return txt;",
  );
  assert_eq!(v, json!("one"), "$ returns first scoped descendant: {v}");

  // $ on a non-matching selector returns null (Playwright parity).
  let v = c.script_value(
    "const p = await page.querySelector('#parent');\
     const r = await p.$('.does-not-exist');\
     await p.dispose();\
     return r === null || r === undefined;",
  );
  assert_eq!(v, json!(true), "$ non-match returns null: {v}");

  // $$ returns every scoped descendant in document order — three `.b`
  // inside #parent, NOT the fourth `.b` sibling outside it. Proves the
  // query is scoped to the handle, not the whole document.
  let v = c.script_value(
    "const p = await page.querySelector('#parent');\
     const els = await p.$$('.b');\
     const texts = [];\
     for (const e of els) { texts.push(await e.textContent()); await e.dispose(); }\
     await p.dispose();\
     return {count: els.length, texts};",
  );
  assert_eq!(v["count"], json!(3), "$$ scoped count excludes sibling: {v}");
  assert_eq!(
    v["texts"],
    json!(["one", "two", "three"]),
    "$$ document-order texts: {v}"
  );

  // $$ with no match returns an empty array, not an error.
  let v = c.script_value(
    "const p = await page.querySelector('#parent');\
     const els = await p.$$('.none');\
     await p.dispose();\
     return els.length;",
  );
  assert_eq!(v, json!(0), "$$ empty match returns empty array: {v}");
}

pub fn test_element_handle_frames(c: &mut McpClient) {
  c.nav("<button id='b'>ok</button>");

  // ownerFrame returns the element's containing frame — the main
  // frame for any connected element on the top-level page.
  let v = c.script_value(
    "const b = await page.querySelector('#b');\
     const fr = await b.ownerFrame();\
     await b.dispose();\
     return fr !== null && fr !== undefined;",
  );
  assert_eq!(v, json!(true), "ownerFrame: {v}");

  // contentFrame returns null for a non-iframe element.
  let v = c.script_value(
    "const b = await page.querySelector('#b');\
     const fr = await b.contentFrame();\
     await b.dispose();\
     return fr === null || fr === undefined;",
  );
  assert_eq!(v, json!(true), "contentFrame non-iframe returns null: {v}");
}

pub fn test_element_handle_waits(c: &mut McpClient) {
  c.nav("<button id='b'>ok</button>");

  // waitForElementState('visible'): already-visible returns fast.
  let v = c.script_value(
    "const b = await page.querySelector('#b');\
     await b.waitForElementState('visible', 5000);\
     await b.dispose();\
     return true;",
  );
  assert_eq!(v, json!(true), "waitForElementState visible: {v}");

  // Element-scoped waitForSelector — polls subtree until non-null.
  c.nav("<div id='p'><span class='inner'>hi</span></div>");
  let v = c.script_value(
    "const p = await page.querySelector('#p');\
     const eh = await p.waitForSelector('.inner', 2000);\
     const ok = eh !== null && eh !== undefined;\
     if (eh) await eh.dispose();\
     await p.dispose();\
     return ok;",
  );
  assert_eq!(v, json!(true), "element-scoped waitForSelector: {v}");
}

pub fn test_element_handle_temp_tag_actions(c: &mut McpClient) {
  // fill
  c.nav("<input id='i' value=''>");
  let v = c.script_value(
    "const eh = await page.querySelector('#i');\
     await eh.fill('hello');\
     const v = await eh.inputValue();\
     await eh.dispose();\
     return v;",
  );
  assert_eq!(v, json!("hello"), "ElementHandle.fill via temp-tag: {v}");

  // check / uncheck
  c.nav("<input type='checkbox' id='c'>");
  let v = c.script_value(
    "const eh = await page.querySelector('#c');\
     await eh.check();\
     const after = await eh.isChecked();\
     await eh.uncheck();\
     const final_ = await eh.isChecked();\
     await eh.dispose();\
     return {after, final_};",
  );
  assert_eq!(v["after"], json!(true), "ElementHandle.check: {v}");
  assert_eq!(v["final_"], json!(false), "ElementHandle.uncheck: {v}");

  // setChecked
  c.nav("<input type='checkbox' id='c'>");
  let v = c.script_value(
    "const eh = await page.querySelector('#c');\
     await eh.setChecked(true);\
     const r = await eh.isChecked();\
     await eh.dispose();\
     return r;",
  );
  assert_eq!(v, json!(true), "ElementHandle.setChecked: {v}");

  // press — target a focused input so the character lands at a
  // predictable spot.
  c.nav("<input id='i' value=''>");
  let v = c.script_value(
    "const eh = await page.querySelector('#i');\
     await eh.press('a');\
     const v = await eh.inputValue();\
     await eh.dispose();\
     return v;",
  );
  assert_eq!(v, json!("a"), "ElementHandle.press: {v}");

  // dispatchEvent — synthetic click fires the page-side handler.
  c.nav("<button id='b' onclick=\"document.title='tt'\">b</button>");
  let v = c.script_value(
    "const eh = await page.querySelector('#b');\
     await eh.dispatchEvent('click');\
     const t = await page.title();\
     await eh.dispose();\
     return t;",
  );
  assert_eq!(v, json!("tt"), "ElementHandle.dispatchEvent click: {v}");

  // selectOption by value.
  c.nav("<select id='s'><option value='a'>A</option><option value='b'>B</option></select>");
  let v = c.script_value(
    "const eh = await page.querySelector('#s');\
     const picked = await eh.selectOption('b');\
     await eh.dispose();\
     return picked;",
  );
  assert_eq!(v, json!(["b"]), "ElementHandle.selectOption: {v}");
}

pub fn test_element_handle_action_options(c: &mut McpClient) {
  // click({ button: 'right' }) — the mousedown handler records the
  // numeric button; right button is 2. A no-option click would record 0.
  c.nav(
    "<button id='b'>b</button>\
     <script>window.__btn=-1;\
     document.getElementById('b').addEventListener('mousedown',e=>{window.__btn=e.button;});\
     </script>",
  );
  let v = c.script_value(
    "const eh = await page.querySelector('#b');\
     await eh.click({ button: 'right' });\
     const b = await page.evaluate('window.__btn');\
     await eh.dispose();\
     return b;",
  );
  // `page.evaluate` JSON-stringifies primitives on the QuickJS boundary.
  let btn = v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok()));
  assert_eq!(btn, Some(2), "ElementHandle.click button:right -> e.button===2: {v}");

  // dblclick() — the dblclick handler only fires on a genuine
  // double-click sequence, so a flag flip proves the two clicks landed.
  c.nav(
    "<button id='b'>b</button>\
     <script>window.__dbl=false;\
     document.getElementById('b').addEventListener('dblclick',()=>{window.__dbl=true;});\
     </script>",
  );
  let v = c.script_value(
    "const eh = await page.querySelector('#b');\
     await eh.dblclick();\
     const d = await page.evaluate('window.__dbl');\
     await eh.dispose();\
     return d;",
  );
  let dbl = v.as_bool().unwrap_or_else(|| v.as_str() == Some("true"));
  assert!(dbl, "ElementHandle.dblclick fires dblclick handler: {v}");

  // hover() — mouseover sets a sentinel that is absent until the
  // pointer moves over the element.
  c.nav(
    "<div id='d' style='width:80px;height:80px'>d</div>\
     <script>window.__hov=false;\
     document.getElementById('d').addEventListener('mouseover',()=>{window.__hov=true;});\
     </script>",
  );
  let v = c.script_value(
    "const eh = await page.querySelector('#d');\
     await eh.hover();\
     const h = await page.evaluate('window.__hov');\
     await eh.dispose();\
     return h;",
  );
  let hov = v.as_bool().unwrap_or_else(|| v.as_str() == Some("true"));
  assert!(hov, "ElementHandle.hover fires mouseover: {v}");

  // type(text, { delay }) — every character lands; `delay` only paces
  // the keystrokes and must not drop input. Observe the full value.
  c.nav("<input id='i' value=''>");
  let v = c.script_value(
    "const eh = await page.querySelector('#i');\
     await eh.focus();\
     await eh.type('xyz', { delay: 1 });\
     const val = await eh.inputValue();\
     await eh.dispose();\
     return val;",
  );
  assert_eq!(v, json!("xyz"), "ElementHandle.type with delay types all chars: {v}");
}

pub fn test_element_handle_select_text(c: &mut McpClient) {
  c.nav("<input id='i' value='abc'>");
  let v = c.script_value(
    "const eh = await page.querySelector('#i');\
     await eh.selectText();\
     const sel = await page.evaluate('document.activeElement && document.activeElement.id');\
     await eh.dispose();\
     return sel;",
  );
  // `page.evaluate` JSON-stringifies strings on the QuickJS boundary
  // — accept either bare or quoted form.
  let s = v.as_str().unwrap_or("");
  assert!(s == "i" || s == "\"i\"", "selectText focuses the input: {v}");
}
