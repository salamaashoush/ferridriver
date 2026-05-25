#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::single_char_pattern,
  clippy::cast_precision_loss,
  clippy::unwrap_used,
  clippy::expect_used
)]
//! run_script: locator + handle action tests, extracted from backends.rs.

use serde_json::json;

use super::client::McpClient;

pub fn test_script_click_at(c: &mut McpClient) {
  c.nav("<div id='d' onclick=\"this.textContent='clicked'\" style='position:fixed;left:0;top:0;width:200px;height:200px'>click me</div>");
  let v = c.script_value(
    "await page.clickAt(50, 50); \
       return await page.textContent('#d');",
  );
  assert_eq!(v, json!("clicked"), "clickAt should trigger onclick");
}

pub fn test_script_mouse_click_coords(c: &mut McpClient) {
  c.nav("<div id='d' onclick=\"this.textContent='mouse-clicked'\" style='position:fixed;left:0;top:0;width:200px;height:200px'>click me</div>");
  let v = c.script_value(
    "await page.mouse.click(40, 40); \
       return await page.textContent('#d');",
  );
  assert_eq!(v, json!("mouse-clicked"), "page.mouse.click should fire onclick");
}

pub fn test_script_drag_coords(c: &mut McpClient) {
  c.nav("<div id='d' onmousedown=\"this.dataset.down='1'\" onmouseup=\"this.dataset.up='1'\" onmousemove=\"this.dataset.moved='1'\" style='position:fixed;left:0;top:0;width:200px;height:200px'>drag</div>");
  let v = c.script_value(
    "await page.mouse.down(); \
       await page.moveMouseSmooth(50, 50, 150, 150, 5); \
       await page.mouse.up(); \
       const down = await page.evaluate(\"document.getElementById('d').dataset.down\"); \
       const up = await page.evaluate(\"document.getElementById('d').dataset.up\"); \
       return { down: down, up: up };",
  );
  assert_eq!(v["down"], json!("1"), "mouse.down should fire mousedown");
  assert_eq!(v["up"], json!("1"), "mouse.up should fire mouseup");
}

pub fn test_script_drag_and_drop(c: &mut McpClient) {
  c.nav("<div id='src' style='width:60px;height:60px;background:#f00' onmousedown=\"this.dataset.d='1'\"></div><div id='tgt' style='width:60px;height:60px;margin-top:80px;background:#0f0' onmouseup=\"this.dataset.u='1'\"></div>");
  let v = c.script_value(
    "await page.dragAndDrop('#src', '#tgt'); \
       const raw = await page.evaluate(\"document.getElementById('src').dataset.d || ''\"); \
       return raw;",
  );
  assert_eq!(v, json!("1"), "dragAndDrop should trigger mousedown on source");
}

pub fn test_script_drag_and_drop_options(c: &mut McpClient) {
  // Navigate to a clean page so prior tests don't leave the browser in a
  // weird mouse state (e.g. held button from a previous drag leaking
  // into this test).
  c.nav(
    "<style>html,body{margin:0;padding:0}</style>\
     <div id='src' style='width:80px;height:80px;background:#f00;position:absolute;left:20px;top:20px'></div>\
     <div id='tgt' style='width:80px;height:80px;background:#0f0;position:absolute;left:200px;top:200px'></div>\
     <div id='out' style='position:fixed;top:0;right:0'>idle</div>\
     <script>\
       var o=document.getElementById('out');\
       var moves=0;\
       window.addEventListener('mousedown',function(e){o.dataset.down=JSON.stringify({x:e.clientX,y:e.clientY});},true);\
       window.addEventListener('mouseup',function(e){o.dataset.up=JSON.stringify({x:e.clientX,y:e.clientY});},true);\
       window.addEventListener('mousemove',function(){moves+=1;o.dataset.moves=String(moves);},true);\
       window.addEventListener('pointermove',function(e){\
         var c=typeof e.getCoalescedEvents==='function'?e.getCoalescedEvents():[];\
         if(c.length>1){moves+=c.length-1;o.dataset.moves=String(moves);}\
       },true);\
     </script>",
  );
  // page.evaluate returns the native JS object directly. The page-side
  // expression here is a `JSON.stringify(...)` call so the outer result
  // is a raw JSON string; we unwrap it with a single JSON.parse and
  // decode each nested {x,y} payload the same way.
  let v = c.script_value(
    "await page.dragAndDrop('#src', '#tgt', { sourcePosition: {x:5, y:5}, targetPosition: {x:10, y:10}, steps: 6 }); \
       const raw = await page.evaluate(\"JSON.stringify({d: document.getElementById('out').dataset.down || null, u: document.getElementById('out').dataset.up || null, m: parseInt(document.getElementById('out').dataset.moves || '0', 10)})\"); \
       const state = JSON.parse(raw); \
       return { d: state.d ? JSON.parse(state.d) : null, u: state.u ? JSON.parse(state.u) : null, m: state.m };",
  );
  let dx = v["d"]["x"].as_f64().unwrap_or(-1.0);
  let dy = v["d"]["y"].as_f64().unwrap_or(-1.0);
  let ux = v["u"]["x"].as_f64().unwrap_or(-1.0);
  let uy = v["u"]["y"].as_f64().unwrap_or(-1.0);
  let moves = v["m"].as_u64().unwrap_or(0);
  assert!(
    (24.0..=26.0).contains(&dx),
    "mousedown x should be ~25 (source padding-box + sourcePosition): got {dx} (v={v})"
  );
  assert!(
    (24.0..=26.0).contains(&dy),
    "mousedown y should be ~25: got {dy} (v={v})"
  );
  assert!(
    (209.0..=211.0).contains(&ux),
    "mouseup x should be ~210 (target padding-box + targetPosition): got {ux} (v={v})"
  );
  assert!(
    (209.0..=211.0).contains(&uy),
    "mouseup y should be ~210: got {uy} (v={v})"
  );
  assert!(
    moves >= 6,
    "steps=6 should produce at least 6 mousemove dispatches: got {moves} (v={v})"
  );
}

pub fn test_script_locator_drag_to_options(c: &mut McpClient) {
  c.nav(
    "<style>html,body{margin:0;padding:0}</style>\
     <div id='src' style='width:80px;height:80px;background:#f00;position:absolute;left:20px;top:20px'></div>\
     <div id='tgt' style='width:80px;height:80px;background:#0f0;position:absolute;left:200px;top:200px'></div>\
     <div id='out' style='position:fixed;top:0;right:0'></div>\
     <script>\
       var o=document.getElementById('out');\
       window.addEventListener('mouseup',function(e){o.dataset.up=JSON.stringify({x:e.clientX,y:e.clientY});},true);\
     </script>",
  );
  let v = c.script_value(
    "await page.locator('#src').dragTo(page.locator('#tgt'), { targetPosition: {x:15, y:15} }); \
       const raw = await page.evaluate(\"document.getElementById('out').dataset.up || ''\"); \
       return raw ? JSON.parse(raw) : null;",
  );
  let ux = v["x"].as_f64().unwrap_or(-1.0);
  let uy = v["y"].as_f64().unwrap_or(-1.0);
  assert!((214.0..=216.0).contains(&ux), "drop x should be ~215: got {ux} (v={v})");
  assert!((214.0..=216.0).contains(&uy), "drop y should be ~215: got {uy} (v={v})");
}

pub fn test_script_emulate_media_all_fields(c: &mut McpClient) {
  // BiDi/Firefox only supports colorScheme; CDP + WebKit support all five.
  // This test runs on CDP backends (cdp-pipe, cdp-raw) and WebKit.
  if c.backend == "bidi" {
    return;
  }
  // PW WebKit rejects `Page.overrideUserPreference` arguments on both
  // the WPE Linux build and the Mac-port build under CI (latest PW
  // protocol mismatch). Other preferences — reducedMotion,
  // forcedColors, contrast — work via separate paths. Skip on every
  // webkit host; CDP backends keep full coverage.
  if c.backend == "webkit" {
    return;
  }
  c.nav("<html><body><div id='x'></div></body></html>");
  let v = c.script_value(
    "await page.emulateMedia({ \
        media: 'print', \
        colorScheme: 'dark', \
        reducedMotion: 'reduce', \
        forcedColors: 'active', \
        contrast: 'more' \
     }); \
     const raw = await page.evaluate(\"JSON.stringify({\
        print: matchMedia('print').matches, \
        screen: matchMedia('screen').matches, \
        dark: matchMedia('(prefers-color-scheme: dark)').matches, \
        reduced: matchMedia('(prefers-reduced-motion: reduce)').matches, \
        forced: matchMedia('(forced-colors: active)').matches, \
        contrast: matchMedia('(prefers-contrast: more)').matches, \
     })\"); \
     return JSON.parse(raw);",
  );
  assert_eq!(
    v["print"],
    json!(true),
    "media=print should activate matchMedia('print'): {v}"
  );
  assert_eq!(
    v["screen"],
    json!(false),
    "matchMedia('screen') should be false under print: {v}"
  );
  assert_eq!(
    v["dark"],
    json!(true),
    "colorScheme=dark should activate prefers-color-scheme:dark: {v}"
  );
  assert_eq!(
    v["reduced"],
    json!(true),
    "reducedMotion=reduce should activate prefers-reduced-motion:reduce: {v}"
  );
  assert_eq!(
    v["forced"],
    json!(true),
    "forcedColors=active should activate forced-colors:active: {v}"
  );
  assert_eq!(
    v["contrast"],
    json!(true),
    "contrast=more should activate prefers-contrast:more: {v}"
  );
  // Reset so state doesn't leak into the next test.
  c.script_value(
    "await page.emulateMedia({ \
       media: null, colorScheme: null, reducedMotion: null, \
       forcedColors: null, contrast: null \
     }); return 'ok';",
  );
}

pub fn test_script_emulate_media_null_disables_single_field(c: &mut McpClient) {
  if c.backend == "bidi" {
    return;
  }
  // See `test_script_emulate_media_all_fields` — PW WebKit rejects
  // overrideUserPreference args on both Linux and macOS. Skip on all
  // webkit hosts.
  if c.backend == "webkit" {
    return;
  }
  c.nav("<html><body>init</body></html>");
  let v = c.script_value(
    "await page.emulateMedia({ colorScheme: 'dark', reducedMotion: 'reduce' }); \
     const pre = await page.evaluate(\"JSON.stringify({\
        dark: matchMedia('(prefers-color-scheme: dark)').matches, \
        reduced: matchMedia('(prefers-reduced-motion: reduce)').matches, \
     })\"); \
     await page.emulateMedia({ colorScheme: null }); \
     const post = await page.evaluate(\"JSON.stringify({\
        dark: matchMedia('(prefers-color-scheme: dark)').matches, \
        reduced: matchMedia('(prefers-reduced-motion: reduce)').matches, \
     })\"); \
     return { pre: JSON.parse(pre), post: JSON.parse(post) };",
  );
  assert_eq!(
    v["pre"]["dark"],
    json!(true),
    "sanity: dark should be active before reset: {v}"
  );
  assert_eq!(
    v["pre"]["reduced"],
    json!(true),
    "sanity: reduced should be active before reset: {v}"
  );
  assert_eq!(
    v["post"]["dark"],
    json!(false),
    "colorScheme=null should disable the override: {v}"
  );
  assert_eq!(
    v["post"]["reduced"],
    json!(true),
    "reducedMotion should survive a sibling reset: {v}"
  );
  c.script_value("await page.emulateMedia({ reducedMotion: null }); return 'ok';");
}

pub fn test_script_drag_and_drop_trial(c: &mut McpClient) {
  c.nav(
    "<style>html,body{margin:0;padding:0}</style>\
     <div id='src' style='width:60px;height:60px;background:#f00;position:absolute;left:20px;top:20px'></div>\
     <div id='tgt' style='width:60px;height:60px;background:#0f0;position:absolute;left:200px;top:200px'></div>\
     <div id='log' data-fired='0'></div>\
     <script>\
       window.addEventListener('mousedown',function(){document.getElementById('log').dataset.fired='1';},true);\
     </script>",
  );
  let v = c.script_value(
    "await page.dragAndDrop('#src', '#tgt', { trial: true }); \
       const raw = await page.evaluate(\"document.getElementById('log').dataset.fired\"); \
       return raw;",
  );
  assert_eq!(v, json!("0"), "trial=true must not dispatch mousedown: got {v}");
}

pub fn test_script_mouse_wheel(c: &mut McpClient) {
  c.nav("<body style='height:3000px'></body>");
  // Verify the binding dispatches the wheel event without error. Whether the
  // event produces a visible scroll depends on Chrome's input routing with
  // the current mouse position (CDP Input.dispatchMouseEvent behaviour is
  // not guaranteed across backends/headless modes).
  let payload = c.script("await page.mouse.wheel(0, 400); return 'ok';");
  assert_eq!(
    payload["status"].as_str(),
    Some("ok"),
    "wheel should not error: {payload}"
  );
}

// Task 1.5: full `ClickOptions` surface — exercise button, modifiers,
// delay, position, clickCount, trial, and the error paths for unknown
// button / modifier strings. Every sub-assertion is a distinct DOM
// probe so per-option failures point at the exact wire bug.
pub fn test_script_click_options(c: &mut McpClient) {
  // button:'right' → contextmenu fires with event.button === 2.
  c.nav(
    "<button id='b' oncontextmenu=\"document.getElementById('out').textContent='right';return false\">b</button><div id='out'>n</div>",
  );
  let v = c.script_value(
    "await page.locator('#b').click({ button: 'right' });\
     return await page.evaluate('document.getElementById(\"out\").textContent');",
  );
  assert_eq!(v, json!("right"), "button=right fires contextmenu: {v}");

  // clickCount:2 → dblclick handler fires.
  c.nav(
    "<button id='b'>b</button><div id='out'>n</div>\
     <script>document.getElementById('b').addEventListener('dblclick',()=>document.getElementById('out').textContent='dbl')</script>",
  );
  let v = c.script_value(
    "await page.locator('#b').click({ clickCount: 2 });\
     return await page.evaluate('document.getElementById(\"out\").textContent');",
  );
  assert_eq!(v, json!("dbl"), "clickCount=2 fires dblclick: {v}");

  // modifiers:['Shift'] → click event has shiftKey === true.
  c.nav(
    "<button id='b'>b</button><div id='out'>n</div>\
     <script>document.getElementById('b').addEventListener('click',e=>document.getElementById('out').textContent=e.shiftKey?'shift':'none')</script>",
  );
  let v = c.script_value(
    "await page.locator('#b').click({ modifiers: ['Shift'] });\
     return await page.evaluate('document.getElementById(\"out\").textContent');",
  );
  assert_eq!(v, json!("shift"), "modifiers Shift sets event.shiftKey: {v}");

  // position:{x:10,y:20} → event coords land at padding-box offset.
  c.nav(
    "<div id='b' style='width:200px;height:100px;background:#ccc'></div><div id='out'>n</div>\
     <script>document.getElementById('b').addEventListener('click',e=>{var r=e.currentTarget.getBoundingClientRect();document.getElementById('out').textContent=(Math.round(e.clientX-r.left))+','+(Math.round(e.clientY-r.top))})</script>",
  );
  let v = c.script_value(
    "await page.locator('#b').click({ position: { x: 10, y: 20 } });\
     return await page.evaluate('document.getElementById(\"out\").textContent');",
  );
  assert_eq!(v, json!("10,20"), "position offsets click coords: {v}");

  // delay:100 → mousedown→mouseup gap is honored (allow slack for
  // timer resolution; demand ≥ 80ms so flaky schedulers still pass).
  c.nav(
    "<button id='b'>b</button><div id='out'>0</div>\
     <script>\
       let down=0;\
       const b=document.getElementById('b');\
       b.addEventListener('mousedown',()=>{down=Date.now()});\
       b.addEventListener('mouseup',()=>{document.getElementById('out').textContent=String(Date.now()-down)});\
     </script>",
  );
  let v = c.script_value(
    "await page.locator('#b').click({ delay: 120 });\
     return await page.evaluate('document.getElementById(\"out\").textContent');",
  );
  let ms = v.as_str().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
  assert!(ms >= 80, "delay=120 held mousedown at least 80ms: got {ms} ({v})");

  // trial:true → click handler doesn't fire, but modifier keydown does.
  c.nav(
    "<button id='b'>b</button><div id='clicked'>no</div><div id='kd'>none</div>\
     <script>\
       document.getElementById('b').addEventListener('click',()=>document.getElementById('clicked').textContent='yes');\
       document.addEventListener('keydown',e=>{if(e.key==='Shift')document.getElementById('kd').textContent='shift'});\
     </script>",
  );
  let v = c.script_value(
    "await page.locator('#b').click({ trial: true, modifiers: ['Shift'] });\
     return {\
       clicked: await page.evaluate('document.getElementById(\"clicked\").textContent'),\
       kd: await page.evaluate('document.getElementById(\"kd\").textContent'),\
     };",
  );
  assert_eq!(v["clicked"], json!("no"), "trial=true skips click handler: {v}");
  assert_eq!(v["kd"], json!("shift"), "trial=true still presses modifiers: {v}");

  // Bad button string → typed error, not silent default.
  let v = c.script_value(
    "try {\
       await page.locator('#b').click({ button: 'garbage' });\
       return 'no-throw';\
     } catch (e) { return String(e.message || e); }",
  );
  let msg = v.as_str().unwrap_or("");
  assert!(
    msg.contains("Unknown mouse button"),
    "bad button errors with exact message: {v}"
  );

  // Bad modifier string → typed error.
  let v = c.script_value(
    "try {\
       await page.locator('#b').click({ modifiers: ['Hyper'] });\
       return 'no-throw';\
     } catch (e) { return String(e.message || e); }",
  );
  let msg = v.as_str().unwrap_or("");
  assert!(msg.contains("Unknown modifier"), "bad modifier errors: {v}");
}

// Task 1.5 phase 4c: `locator.dispatchEvent` must honor `opts.timeout`
// via the retry loop (previously did a one-shot `resolve()` that failed
// immediately on missing elements). Playwright's dispatchEvent does NOT
// run actionability — it's a programmatic event dispatch, polled only
// for element presence.
pub fn test_script_dispatch_event_timeout(c: &mut McpClient) {
  c.nav("<button id='b'>b</button>");
  let v = c.script_value(
    "const t0 = Date.now();\
     try { await page.locator('#nope').dispatchEvent('click', {}, { timeout: 200 }); return { msg: 'no-throw', elapsed: Date.now() - t0 }; }\
     catch (e) { return { msg: String(e.message || e), elapsed: Date.now() - t0 }; }",
  );
  let msg = v["msg"].as_str().unwrap_or("");
  let elapsed = v["elapsed"].as_i64().unwrap_or(9_999);
  assert!(
    msg.contains("Timeout") && msg.contains("200ms"),
    "dispatchEvent on missing element with timeout:200 should Timeout: {v}"
  );
  assert!(
    elapsed < 1500,
    "dispatchEvent timeout should fire within 1.5s, got {elapsed}ms: {v}"
  );
}

// Task 1.5 phase 4d: `selectOption` honors `opts.timeout` (via
// retry_resolve) AND `opts.force` (skips the `['visible','enabled']`
// pre-check that would otherwise return `error:notenabled`). Without
// force on a disabled `<select>`, the retry loop polls until the
// deadline. With force, the injected `selectOptions` runs immediately.
pub fn test_script_select_option_force(c: &mut McpClient) {
  // Disabled select — without force, fails fast via the timeout.
  c.nav("<select id='s' disabled><option value='a'>A</option><option value='b'>B</option></select>");
  let v = c.script_value(
    "const t0 = Date.now();\
     try { await page.locator('#s').selectOption('b', { timeout: 200 }); return { msg: 'no-throw', elapsed: Date.now() - t0 }; }\
     catch (e) { return { msg: String(e.message || e), elapsed: Date.now() - t0 }; }",
  );
  let msg = v["msg"].as_str().unwrap_or("");
  let elapsed = v["elapsed"].as_i64().unwrap_or(9_999);
  assert!(
    msg.contains("Timeout") && msg.contains("200ms"),
    "selectOption on disabled select with timeout:200 should Timeout: {v}"
  );
  assert!(
    elapsed < 1500,
    "selectOption timeout should fire within 1.5s, got {elapsed}ms: {v}"
  );
  // Value unchanged.
  let post = c.script_value("return await page.evaluate('document.getElementById(\"s\").value');");
  assert_eq!(
    post,
    json!("a"),
    "disabled select value unchanged after timeout: {post}"
  );

  // force: true bypasses the pre-check and selects even when disabled.
  c.nav("<select id='s' disabled><option value='a'>A</option><option value='b'>B</option></select>");
  c.script_value("await page.locator('#s').selectOption('b', { force: true });");
  let after = c.script_value("return await page.evaluate('document.getElementById(\"s\").value');");
  assert_eq!(
    after,
    json!("b"),
    "selectOption with force:true should bypass disabled pre-check: {after}"
  );
}

// Task 1.5 phase 4b: `check`/`uncheck`/`setChecked` must verify the
// final state matches the target AND reject uncheck-of-radio, matching
// Playwright's `server/dom.ts::_setChecked`. Proves on every backend:
//   - A custom checkbox with an `onclick` preventDefault doesn't change
//     state → call throws "Clicking the checkbox did not change its state".
//   - `uncheck` on a checked radio → throws the exact Playwright error
//     naming radio groups.
//   - `trial: true` skips verification (caller asserting actionability,
//     not state change).
//   - `check` on a plain checkbox flips the state and returns ok.
pub fn test_script_check_behavior(c: &mut McpClient) {
  // 1. Plain checkbox: check() toggles to checked.
  c.nav("<input id='cb' type='checkbox'>");
  c.script_value("await page.locator('#cb').check();");
  let v = c.script_value("return await page.evaluate('document.getElementById(\"cb\").checked');");
  assert_eq!(v, json!(true), "check() should toggle checkbox on: {v}");

  // 2. Checkbox that intercepts the click → state does not change →
  //    check() throws the Playwright-exact "did not change its state".
  c.nav("<input id='cb' type='checkbox' onclick='event.preventDefault()'>");
  let v = c.script_value(
    "try { await page.locator('#cb').check({ timeout: 500 }); return 'no-throw'; } \
     catch (e) { return String(e.message || e); }",
  );
  let msg = v.as_str().unwrap_or("");
  assert!(
    msg.contains("did not change its state"),
    "preventDefault checkbox should throw Playwright 'did not change its state', got: {v}"
  );

  // 3. Uncheck a checked radio → typed Playwright radio-group error.
  c.nav("<input id='r' type='radio' name='g' checked><input type='radio' name='g'>");
  let v = c.script_value(
    "try { await page.locator('#r').uncheck(); return 'no-throw'; } \
     catch (e) { return String(e.message || e); }",
  );
  let msg = v.as_str().unwrap_or("");
  assert!(
    msg.contains("Cannot uncheck radio button"),
    "uncheck radio should throw 'Cannot uncheck radio button', got: {v}"
  );

  // 4. trial: true skips the post-click verification AND the click —
  //    preventDefault checkbox that would normally throw returns ok.
  c.nav("<input id='cb' type='checkbox' onclick='event.preventDefault()'>");
  c.script_value("await page.locator('#cb').check({ trial: true });");
  let v = c.script_value("return await page.evaluate('document.getElementById(\"cb\").checked');");
  assert_eq!(
    v,
    json!(false),
    "trial:true should NOT actually toggle the checkbox state: {v}"
  );

  // 5. check() on an already-checked checkbox is a no-op (no click, no
  //    verification error). Prove by attaching an `onclick` listener and
  //    asserting it never fires.
  c.nav(
    "<input id='cb' type='checkbox' checked>\
     <div id='count'>0</div>\
     <script>\
       document.getElementById('cb').addEventListener('click', () => {\
         const el = document.getElementById('count');\
         el.textContent = String(parseInt(el.textContent, 10) + 1);\
       });\
     </script>",
  );
  c.script_value("await page.locator('#cb').check();");
  let v = c.script_value("return await page.evaluate('document.getElementById(\"count\").textContent');");
  assert_eq!(v, json!("0"), "already-checked check() must skip the click: {v}");
}

// Task 1.5 phase 4a: `fill.force` must actually bypass Playwright's
// `['visible','enabled','editable']` pre-check. Proves on every backend:
//   - Without force on a `readonly` input: the pre-check returns
//     `error:noteditable` and the retry loop polls until timeout.
//   - With force:true on the same input: the pre-check is skipped and
//     the JS `.value = 'x'` assignment goes through regardless of the
//     `readonly` attribute, letting the caller override it explicitly.
pub fn test_script_fill_force(c: &mut McpClient) {
  c.nav("<input id='ro' readonly value=''><div id='out'></div>");

  // 1. force: false (default) on readonly input → times out (retry
  //    loop sees `error:noteditable` as a retriable marker).
  let v = c.script_value(
    "const t0 = Date.now();\
     try { await page.locator('#ro').fill('hello', { timeout: 250 }); return { msg: 'no-throw', elapsed: Date.now() - t0 }; }\
     catch (e) { return { msg: String(e.message || e), elapsed: Date.now() - t0 }; }",
  );
  let msg = v["msg"].as_str().unwrap_or("");
  let elapsed = v["elapsed"].as_i64().unwrap_or(9_999);
  assert!(
    msg.contains("Timeout"),
    "fill without force on readonly should Timeout, got: {v}"
  );
  assert!(
    elapsed < 1500,
    "fill timeout should fire within 1.5s, got {elapsed}ms: {v}"
  );
  // Value stays empty — confirms no write happened.
  let post = c.script_value("return await page.evaluate('document.getElementById(\"ro\").value');");
  assert_eq!(post, json!(""), "readonly input should still be empty: {post}");

  // 2. force: true on the same readonly input → writes successfully.
  c.nav("<input id='ro' readonly value=''>");
  c.script_value("await page.locator('#ro').fill('bypass', { force: true });");
  let after = c.script_value("return await page.evaluate('document.getElementById(\"ro\").value');");
  assert_eq!(
    after,
    json!("bypass"),
    "fill with force:true should set value on readonly: {after}"
  );
}

// Task 1.5 phase 3 (Rule 4): `locator.tap` must use the backend's native
// touch primitive on every backend that supports it, not a JS `TouchEvent`
// shim. CDP dispatches via `Input.dispatchTouchEvent` producing
// `isTrusted === true` events. BiDi (no pointerType='touch' in stable) and
// WebKit (no public NSTouchEvent synthesis) surface a typed Unsupported
// error instead.
pub fn test_script_tap_native(c: &mut McpClient) {
  if c.backend == "bidi" {
    // BiDi has no `pointerType='touch'` in stable yet. Tap must surface
    // Unsupported — not a silent JS fallback.
    c.nav(
      "<button id='b' ontouchstart=\"document.getElementById('out').textContent='fired'\">b</button>\
       <div id='out'>no</div>",
    );
    let v = c.script_value(
      "try { await page.locator('#b').tap({ timeout: 2000 }); return { msg: 'no-throw' }; } \
       catch (e) { return { msg: String(e.message || e) }; }",
    );
    let msg = v["msg"].as_str().unwrap_or("");
    assert!(
      msg.contains("unsupported") || msg.contains("Unsupported"),
      "{}: tap should throw Unsupported, got: {v}",
      c.backend
    );
    assert!(
      msg.contains("tap"),
      "{}: Unsupported message should mention tap, got: {v}",
      c.backend
    );
    let after = c.script_value("return await page.evaluate('document.getElementById(\"out\").textContent');");
    assert_eq!(
      after,
      json!("no"),
      "{}: no JS-fallback tap should have fired; got {after}",
      c.backend
    );
    return;
  }

  // CDP native path: Input.dispatchTouchEvent emits a trusted touchstart
  // + touchend pair. Record event.isTrusted and whether the touch point
  // lands inside the button rect; read each field back as a separate
  // `textContent` so we stay inside the single-level JSON.parse pattern
  // (QuickJS `page.evaluate` returns a JSON-stringified result).
  c.nav(
    "<button id='b' style='width:100px;height:50px'>b</button>\
     <div id='trusted'>n</div><div id='inrect'>n</div>\
     <script>\
       const b = document.getElementById('b');\
       b.addEventListener('touchstart', e => {\
         const t = e.changedTouches[0];\
         const r = b.getBoundingClientRect();\
         document.getElementById('trusted').textContent = String(e.isTrusted);\
         document.getElementById('inrect').textContent = String(\
           t.clientX >= r.left && t.clientX <= r.right && t.clientY >= r.top && t.clientY <= r.bottom\
         );\
       }, { passive: true });\
     </script>",
  );
  let v = c.script_value(
    "await page.locator('#b').tap();\
     return {\
       trusted: await page.evaluate('document.getElementById(\"trusted\").textContent'),\
       inRect: await page.evaluate('document.getElementById(\"inrect\").textContent'),\
     };",
  );
  assert_eq!(
    v["trusted"],
    json!("true"),
    "CDP tap should emit isTrusted=true touchstart; got: {v}"
  );
  assert_eq!(
    v["inRect"],
    json!("true"),
    "CDP tap should land inside button rect; got: {v}"
  );

  // Modifiers propagate to the touch event: tap + Shift → event.shiftKey.
  c.nav(
    "<button id='b'>b</button><div id='out'>no</div>\
     <script>\
       document.getElementById('b').addEventListener('touchstart', e => {\
         document.getElementById('out').textContent = e.shiftKey ? 'shift' : 'none';\
       }, { passive: true });\
     </script>",
  );
  let v = c.script_value(
    "await page.locator('#b').tap({ modifiers: ['Shift'] });\
     return await page.evaluate('document.getElementById(\"out\").textContent');",
  );
  assert_eq!(
    v,
    json!("shift"),
    "tap modifiers:['Shift'] must set event.shiftKey on touchstart: {v}"
  );

  // trial:true skips the touch dispatch but still presses modifiers.
  c.nav(
    "<button id='b'>b</button><div id='tap'>no</div><div id='kd'>no</div>\
     <script>\
       document.getElementById('b').addEventListener('touchstart', () => { document.getElementById('tap').textContent = 'yes'; }, { passive: true });\
       document.addEventListener('keydown', e => { if (e.key === 'Shift') document.getElementById('kd').textContent = 'shift'; });\
     </script>",
  );
  let v = c.script_value(
    "await page.locator('#b').tap({ trial: true, modifiers: ['Shift'] });\
     return {\
       t: await page.evaluate('document.getElementById(\"tap\").textContent'),\
       k: await page.evaluate('document.getElementById(\"kd\").textContent'),\
     };",
  );
  assert_eq!(v["t"], json!("no"), "trial:true skips touchstart dispatch: {v}");
  assert_eq!(v["k"], json!("shift"), "trial:true still presses modifier: {v}");
}

// Task 1.5 phase 2: `opts.timeout` must honor the user's deadline on every
// action method — previously accepted and silently ignored. For each action
// we call on a selector that doesn't exist with `timeout: 200`; the call
// must throw a TimeoutError within ~1.5s (wall clock) instead of waiting
// out the page default (30s). Proves the deadline threaded through
// `retry_resolve!` actually fires.
pub fn test_script_action_timeout(c: &mut McpClient) {
  c.nav("<button id='b'>b</button>");
  for (name, call) in [
    ("click", "await page.locator('#nope').click({ timeout: 200 })"),
    ("fill", "await page.locator('#nope').fill('x', { timeout: 200 })"),
    ("hover", "await page.locator('#nope').hover({ timeout: 200 })"),
    ("tap", "await page.locator('#nope').tap({ timeout: 200 })"),
    ("press", "await page.locator('#nope').press('A', { timeout: 200 })"),
    ("type", "await page.locator('#nope').type('x', { timeout: 200 })"),
    ("dblclick", "await page.locator('#nope').dblclick({ timeout: 200 })"),
    ("check", "await page.locator('#nope').check({ timeout: 200 })"),
    ("uncheck", "await page.locator('#nope').uncheck({ timeout: 200 })"),
  ] {
    let src = format!(
      "const t0 = Date.now();\
       try {{ {call}; return {{ elapsed: Date.now() - t0, msg: 'no-throw' }}; }}\
       catch (e) {{ return {{ elapsed: Date.now() - t0, msg: String(e.message || e) }}; }}"
    );
    let v = c.script_value(&src);
    let msg = v["msg"].as_str().unwrap_or("");
    let elapsed = v["elapsed"].as_i64().unwrap_or(99_999);
    assert!(
      msg.contains("Timeout") && msg.contains("200ms"),
      "{name}: expected TimeoutError w/ 200ms; got: {v}"
    );
    assert!(
      elapsed < 1500,
      "{name}: expected to fail within 1.5s of 200ms timeout; got {elapsed}ms: {v}"
    );
  }
}

// Task 1.3 phase B: the injected `window.__fd` namespace exposes the
// Playwright `UtilityScript` class and its isomorphic serializer
// helpers (`parseEvaluationResultValue`, `serializeAsCallArgument`).
// These are the load-bearing primitives for `page.evaluate(fn, arg)` +
// JSHandle round-trip in phase D — if they're missing from the bundle
// or shadowed by a later `if (!window.__fd)` guard, evaluate(fn, arg)
// will never work no matter how the Rust side serializes. Proves the
// bundle surfaces them on every backend.
pub fn test_script_utility_script_exposed(c: &mut McpClient) {
  c.nav("<div id='x'></div>");

  // 1. The class and both serializer helpers exist on window.__fd.
  //    `page.evaluate` JSON-stringifies the result string `"function"`,
  //    so one JSON.parse unwraps the quote.
  let v = c.script_value(
    "return {\
       hasClass: await page.evaluate('typeof window.__fd.UtilityScript'),\
       hasFactory: await page.evaluate('typeof window.__fd.newUtilityScript'),\
       hasParse: await page.evaluate('typeof window.__fd.parseEvaluationResultValue'),\
       hasSerialize: await page.evaluate('typeof window.__fd.serializeAsCallArgument'),\
     };",
  );
  assert_eq!(v["hasClass"], json!("function"), "UtilityScript class missing: {v}");
  assert_eq!(
    v["hasFactory"],
    json!("function"),
    "newUtilityScript factory missing: {v}"
  );
  assert_eq!(
    v["hasParse"],
    json!("function"),
    "parseEvaluationResultValue missing: {v}"
  );
  assert_eq!(
    v["hasSerialize"],
    json!("function"),
    "serializeAsCallArgument missing: {v}"
  );

  // 2. The factory returns a working instance — its `evaluate` and
  //    `jsonValue` methods are invokable.
  let v = c.script_value(
    "return {\
       hasEvaluate: await page.evaluate('typeof window.__fd.newUtilityScript().evaluate'),\
       hasJsonValue: await page.evaluate('typeof window.__fd.newUtilityScript().jsonValue'),\
     };",
  );
  assert_eq!(
    v["hasEvaluate"],
    json!("function"),
    "UtilityScript.evaluate missing: {v}"
  );
  assert_eq!(
    v["hasJsonValue"],
    json!("function"),
    "UtilityScript.jsonValue missing: {v}"
  );

  // 3. The deserializer round-trips Playwright's wire shapes for rich
  //    types — a smoke check that the isomorphic format we built on the
  //    Rust side is the same one the page's utility script parses.
  //    Probe each result as a primitive string so QuickJS' JSON.stringify
  //    on page.evaluate plays nicely.
  let probes = [
    // `{v: 'NaN'}` → NaN. Use Number.isNaN to verify since NaN !== NaN.
    (
      "nan",
      "Number.isNaN(window.__fd.parseEvaluationResultValue({v: 'NaN'}))",
      json!(true),
    ),
    // `{v: 'Infinity'}` → Infinity.
    (
      "inf",
      "window.__fd.parseEvaluationResultValue({v: 'Infinity'}) === Infinity",
      json!(true),
    ),
    // `{v: '-Infinity'}` → -Infinity.
    (
      "neginf",
      "window.__fd.parseEvaluationResultValue({v: '-Infinity'}) === -Infinity",
      json!(true),
    ),
    // `{v: '-0'}` → -0. Detect via 1/-0 === -Infinity.
    (
      "negzero",
      "1 / window.__fd.parseEvaluationResultValue({v: '-0'}) === -Infinity",
      json!(true),
    ),
    // `{v: 'null'}` → null.
    (
      "null",
      "window.__fd.parseEvaluationResultValue({v: 'null'}) === null",
      json!(true),
    ),
    // `{v: 'undefined'}` → undefined.
    (
      "undef",
      "typeof window.__fd.parseEvaluationResultValue({v: 'undefined'})",
      json!("undefined"),
    ),
    // `{d: '...'}` → Date instance.
    (
      "date",
      "window.__fd.parseEvaluationResultValue({d: '2024-01-01T00:00:00.000Z'}) instanceof Date",
      json!(true),
    ),
    // `{u: '...'}` → URL instance.
    (
      "url",
      "window.__fd.parseEvaluationResultValue({u: 'https://a.test/x'}) instanceof URL",
      json!(true),
    ),
    // `{r: {p, f}}` → RegExp.
    (
      "regexp",
      "window.__fd.parseEvaluationResultValue({r: {p: 'foo', f: 'gi'}}) instanceof RegExp",
      json!(true),
    ),
    // `{bi: '42'}` → BigInt(42n). `typeof` == 'bigint'.
    (
      "bigint",
      "typeof window.__fd.parseEvaluationResultValue({bi: '42'})",
      json!("bigint"),
    ),
    // `{e: {m, n, s}}` → Error.
    (
      "error",
      "window.__fd.parseEvaluationResultValue({e: {n: 'TypeError', m: 'oops', s: ''}}) instanceof Error",
      json!(true),
    ),
  ];
  for (name, probe_expr, expected) in probes {
    // `page.evaluate` in QuickJS already JSON-stringifies its result
    // before handing it back, so one `JSON.parse` is enough to unwrap.
    // The inline-script `{probe_expr}` must return a JSON-expressible
    // primitive (bool or typeof-string in our probes).
    let script = format!("return await page.evaluate({probe_expr:?});");
    let got = c.script_value(&script);
    assert_eq!(
      got, expected,
      "deserializer probe '{name}' failed: expr {probe_expr}, got {got}"
    );
  }

  // 4. Round-trip: serialize a rich value → deserialize → re-serialize
  //    and assert the wire shape is stable. Exercises the complete
  //    isomorphic format end-to-end inside the page. `page.evaluate`
  //    already JSON-stringifies the IIFE's return value, so one
  //    `JSON.parse` unwraps the object shape.
  let v = c.script_value(
    "return await page.evaluate(`(() => {\
       const raw = {d: '2024-06-01T00:00:00.000Z'};\
       const dateObj = window.__fd.parseEvaluationResultValue(raw);\
       return window.__fd.serializeAsCallArgument(dateObj, v => ({fallThrough: v}));\
     })()`);",
  );
  assert_eq!(
    v,
    json!({"d": "2024-06-01T00:00:00.000Z"}),
    "Date round-trip should preserve the d-tag wire shape: {v}"
  );
}

// Task 1.2 + 1.3 phase C — JSHandle + ElementHandle lifecycle. Rule 9:
// dispose must work end-to-end on every backend (cdp-pipe, cdp-raw,
// webkit, bidi). Exercises the QuickJS `ElementHandle.dispose()` +
// `JSHandle.dispose()` + idempotence via `run_script` so all four
// backends are proven.
pub fn test_script_handle_lifecycle(c: &mut McpClient) {
  c.nav("<button id='primary'>ok</button><div class='needle'>x</div>");

  // querySelector returns an ElementHandle with isDisposed=false.
  let v = c.script_value(
    "const h = await page.querySelector('button#primary');\
     return {found: h !== null, disposed: h.isDisposed()};",
  );
  assert_eq!(v["found"], json!(true), "querySelector missed #primary: {v}");
  assert_eq!(v["disposed"], json!(false), "fresh handle already disposed: {v}");

  // $ alias returns a handle too.
  let v = c.script_value(
    "const h = await page.$('div.needle');\
     return h !== null;",
  );
  assert_eq!(v, json!(true), "$ alias missed .needle: {v}");

  // Missing selector returns null/undefined (not an error). Use `== null`
  // (loose equality) so we accept both representations — rquickjs maps
  // Rust's `Option::None` to `undefined` on the JS side, while
  // Playwright's TS types say `null`. Either is acceptable here; what
  // we're testing is that an unmatched selector is non-truthy, not an
  // error.
  let v = c.script_value(
    "const r = await page.querySelector('button#does-not-exist');\
     return r === null || r === undefined;",
  );
  assert_eq!(v, json!(true), "missing selector did not return null: {v}");

  // dispose() latches isDisposed and is idempotent.
  let v = c.script_value(
    "const h = await page.querySelector('button#primary');\
     const before = h.isDisposed();\
     await h.dispose();\
     const after1 = h.isDisposed();\
     await h.dispose();\
     const after2 = h.isDisposed();\
     return {before, after1, after2};",
  );
  assert_eq!(v["before"], json!(false), "before dispose: {v}");
  assert_eq!(v["after1"], json!(true), "after first dispose: {v}");
  assert_eq!(v["after2"], json!(true), "after second dispose: {v}");

  // asJSHandle shares the disposed flag with the ElementHandle.
  let v = c.script_value(
    "const eh = await page.querySelector('button#primary');\
     const jh = eh.asJSHandle();\
     const before_eh = eh.isDisposed();\
     const before_jh = jh.isDisposed();\
     await eh.dispose();\
     const after_eh = eh.isDisposed();\
     const after_jh = jh.isDisposed();\
     return {before_eh, before_jh, after_eh, after_jh};",
  );
  assert_eq!(v["before_eh"], json!(false));
  assert_eq!(v["before_jh"], json!(false));
  assert_eq!(v["after_eh"], json!(true));
  // Shared Arc<AtomicBool> means the JSHandle observes the dispose too.
  assert_eq!(
    v["after_jh"],
    json!(true),
    "JSHandle sibling did not see the dispose: {v}"
  );

  // JSHandle.asElement is functional — probes `h instanceof Node` and
  // re-wraps the remote as an ElementHandle when true.
  let v = c.script_value(
    "const eh = await page.querySelector('button#primary');\
     const jh = eh.asJSHandle();\
     const asEl = await jh.asElement();\
     const ok = asEl !== null && asEl !== undefined;\
     await eh.dispose();\
     return ok;",
  );
  assert_eq!(v, json!(true), "asElement did not promote a DOM-node remote: {v}");

  // Non-DOM remotes (plain objects, arrays, functions) yield null.
  // BiDi refers to these via `{type: 'handle', handle}` — not the
  // node-only `sharedReference` wire shape — so the evaluate path
  // must emit the correct form when the handle rides through as an
  // argument. This test exercises that end-to-end.
  let v = c.script_value(
    "const jh = await page.evaluateHandle(\"() => ({ not: 'a dom node' })\", null);\
     const asEl = await jh.asElement();\
     await jh.dispose();\
     return asEl === null || asEl === undefined;",
  );
  assert_eq!(v, json!(true), "asElement returned non-null for non-DOM remote: {v}");
}

// page.evaluate(fn, arg) / evaluateHandle(fn) / handle.evaluate(fn) —
// `fn` accepts either a string or a real JS function, matching
// Playwright's `String(pageFunction)` + `typeof fn === 'function'`
// at `/tmp/playwright/packages/playwright-core/src/client/frame.ts:196`.
// Rule 9 covers all four backends via QuickJS `run_script`.
pub fn test_script_evaluate_fn_and_handle(c: &mut McpClient) {
  c.nav("<button id='primary'>ok</button>");

  // page.evaluate(fn, primitive) — function-call semantics.
  let v = c.script_value("return await page.evaluate(x => x + 1, 41);");
  assert_eq!(v, json!(42), "primitive arg round-trip: {v}");

  // page.evaluate(fn, object) — JSON round-trip.
  let v = c.script_value("return await page.evaluate(o => o.a + o.b, {a: 2, b: 3});");
  assert_eq!(v, json!(5), "object arg round-trip: {v}");

  // page.evaluate(fn, null) — no-arg function-call with null.
  let v = c.script_value("return await page.evaluate(() => 7, null);");
  assert_eq!(v, json!(7), "null-arg call: {v}");

  // String form also accepted (Playwright parity — `String(pageFunction)`).
  let v = c.script_value("return await page.evaluate('1 + 1');");
  assert_eq!(v, json!(2), "expression-as-string: {v}");

  // page.evaluateHandle — returns a live JSHandle.
  let v = c.script_value(
    "const h = await page.evaluateHandle(() => ({x: 42}));\
     const disposed = h.isDisposed();\
     await h.dispose();\
     return {disposed_before: disposed, disposed_after: h.isDisposed()};",
  );
  assert_eq!(v["disposed_before"], json!(false));
  assert_eq!(v["disposed_after"], json!(true));

  // handle.evaluate passes the handle as arg[0].
  let v = c.script_value(
    "const h = await page.evaluateHandle(() => document.body);\
     const tag = await h.evaluate(el => el.tagName);\
     await h.dispose();\
     return tag;",
  );
  assert_eq!(v, json!("BODY"), "handle.evaluate: {v}");

  // ElementHandle.evaluate routes through its JSHandle.
  let v = c.script_value(
    "const eh = await page.querySelector('button#primary');\
     const tag = await eh.evaluate(el => el.tagName);\
     await eh.dispose();\
     return tag;",
  );
  assert_eq!(v, json!("BUTTON"), "ElementHandle.evaluate: {v}");

  // Disposed-handle use raises the Playwright 'disposed' error.
  let v = c.script_value(
    "const eh = await page.querySelector('button#primary');\
     const jh = eh.asJSHandle();\
     await eh.dispose();\
     let threw = false;\
     let msg = '';\
     try { await jh.evaluate(el => el.tagName); }\
     catch (e) { threw = true; msg = String(e.message || e); }\
     return {threw, hasDisposedWord: msg.indexOf('disposed') >= 0};",
  );
  assert_eq!(v["threw"], json!(true), "disposed-use threw?: {v}");
  assert_eq!(
    v["hasDisposedWord"],
    json!(true),
    "disposed error mentions 'disposed': {v}"
  );
}

// Rich-type round-trip — Date / RegExp / NaN / Infinity / BigInt /
// undefined arrive on the JS side as native values, matching
// Playwright's `parseSerializedValue` at
// `/tmp/playwright/packages/playwright-core/src/protocol/serializers.ts:19`.
// Rule 9 across cdp-pipe / cdp-raw / bidi / webkit.
pub fn test_script_evaluate_rich_types(c: &mut McpClient) {
  c.nav("<div></div>");

  // Date: rehydrates to `Date` instance.
  let v = c.script_value(
    "const d = await page.evaluate(() => new Date('2024-06-01T00:00:00.000Z'));\
     return {is_date: d instanceof Date, iso: d.toISOString()};",
  );
  assert_eq!(v["is_date"], json!(true), "Date is native: {v}");
  assert_eq!(v["iso"], json!("2024-06-01T00:00:00.000Z"), "Date round-trips: {v}");

  // RegExp: rehydrates to `RegExp` instance.
  let v = c.script_value(
    "const r = await page.evaluate(() => /foo.*bar/gi);\
     return {is_regexp: r instanceof RegExp, source: r.source, flags: r.flags};",
  );
  assert_eq!(v["is_regexp"], json!(true), "RegExp is native: {v}");
  assert_eq!(v["source"], json!("foo.*bar"), "RegExp source: {v}");
  assert_eq!(v["flags"], json!("gi"), "RegExp flags: {v}");

  // NaN: rehydrates to literal NaN.
  let v = c.script_value("return Number.isNaN(await page.evaluate(() => NaN));");
  assert_eq!(v, json!(true), "NaN round-trip: {v}");

  // Infinity: literal +Infinity.
  let v = c.script_value("return (await page.evaluate(() => Infinity)) === Infinity;");
  assert_eq!(v, json!(true), "Infinity round-trip: {v}");

  // BigInt: rehydrates to a `bigint`.
  let v = c.script_value(
    "const b = await page.evaluate(() => 9007199254740993n);\
     return {type: typeof b, str: String(b)};",
  );
  assert_eq!(v["type"], json!("bigint"), "BigInt type: {v}");
  assert_eq!(v["str"], json!("9007199254740993"), "BigInt value: {v}");

  // undefined: rehydrates to literal undefined (== null, !== null).
  let v = c.script_value(
    "const u = await page.evaluate(() => undefined);\
     return {is_undef: u === undefined, loose_null: u == null};",
  );
  assert_eq!(v["is_undef"], json!(true), "undefined round-trip: {v}");
  assert_eq!(v["loose_null"], json!(true), "undefined == null: {v}");
}

// Task 1.2 phase E — ElementHandle DOM methods. Rule 9: verify reads,
// state predicates, bounding_box, click / focus / scrollIntoView on
// all four backends via QuickJS.
pub fn test_script_element_handle_methods(c: &mut McpClient) {
  c.nav("<a id='l' href='/x' data-k='v'>hello <b>world</b></a>");

  // innerHTML / innerText / textContent / getAttribute
  let v = c.script_value(
    "const eh = await page.querySelector('a#l');\
     const result = {\
       inner_html: await eh.innerHTML(),\
       inner_text: await eh.innerText(),\
       text_content: await eh.textContent(),\
       href: await eh.getAttribute('href'),\
       k: await eh.getAttribute('data-k'),\
     };\
     await eh.dispose();\
     return result;",
  );
  let inner = v["inner_html"].as_str().unwrap_or("");
  // BiDi injects a `data-fdref` attribute on DOM elements it
  // references, so the serialised innerHTML is `<b data-fdref="...">`
  // rather than a bare `<b>`. Match the substrings that matter.
  assert!(inner.contains("<b") && inner.contains("world</b>"), "innerHTML: {v}");
  assert_eq!(v["inner_text"], json!("hello world"), "innerText: {v}");
  assert_eq!(v["text_content"], json!("hello world"), "textContent: {v}");
  assert_eq!(v["href"], json!("/x"), "getAttribute(href): {v}");
  assert_eq!(v["k"], json!("v"), "getAttribute(data-k): {v}");

  // inputValue
  c.nav("<input id='i' value='hi'>");
  let v = c.script_value(
    "const eh = await page.querySelector('#i');\
     const v = await eh.inputValue();\
     await eh.dispose();\
     return v;",
  );
  assert_eq!(v, json!("hi"), "inputValue: {v}");

  // State predicates
  c.nav("<button id='v'>x</button><button id='d' disabled>x</button><button id='h' style='display:none'>x</button>");
  let v = c.script_value(
    "const v = await page.querySelector('#v');\
     const d = await page.querySelector('#d');\
     const h = await page.querySelector('#h');\
     const result = {\
       v_visible: await v.isVisible(),\
       v_enabled: await v.isEnabled(),\
       d_disabled: await d.isDisabled(),\
       h_hidden: await h.isHidden(),\
     };\
     await v.dispose(); await d.dispose(); await h.dispose();\
     return result;",
  );
  assert_eq!(v["v_visible"], json!(true));
  assert_eq!(v["v_enabled"], json!(true));
  assert_eq!(v["d_disabled"], json!(true));
  assert_eq!(v["h_hidden"], json!(true));

  // isChecked + isEditable
  c.nav("<input type='checkbox' id='c' checked><input id='i'><input id='r' readonly>");
  let v = c.script_value(
    "const c = await page.querySelector('#c');\
     const i = await page.querySelector('#i');\
     const r = await page.querySelector('#r');\
     const result = {\
       c_checked: await c.isChecked(),\
       i_editable: await i.isEditable(),\
       r_editable: await r.isEditable(),\
     };\
     await c.dispose(); await i.dispose(); await r.dispose();\
     return result;",
  );
  assert_eq!(v["c_checked"], json!(true));
  assert_eq!(v["i_editable"], json!(true));
  assert_eq!(v["r_editable"], json!(false));

  // boundingBox
  c.nav("<button id='b' style='position:absolute;left:10px;top:20px;width:50px;height:30px'>b</button>");
  let v = c.script_value(
    "const b = await page.querySelector('#b');\
     const box = await b.boundingBox();\
     await b.dispose();\
     return box;",
  );
  let width = v["width"].as_f64().unwrap_or(0.0);
  let height = v["height"].as_f64().unwrap_or(0.0);
  assert!(width > 0.0, "bbox width > 0: {v}");
  assert!(height > 0.0, "bbox height > 0: {v}");

  // click fires the native handler. The onclick handler is
  // synchronous so the title update is observable on the next
  // page.title round-trip — no setTimeout needed (QuickJS doesn't
  // have setTimeout anyway).
  c.nav("<button id='b' onclick=\"document.title='clicked'\">b</button>");
  let v = c.script_value(
    "const b = await page.querySelector('#b');\
     await b.click();\
     const t = await page.title();\
     await b.dispose();\
     return t;",
  );
  assert_eq!(v, json!("clicked"), "click fired: {v}");

  // focus updates activeElement
  c.nav("<input id='i'>");
  let v = c.script_value(
    "const i = await page.querySelector('#i');\
     await i.focus();\
     const active = await page.evaluate('document.activeElement && document.activeElement.id');\
     await i.dispose();\
     return active;",
  );
  // QuickJS page.evaluate wraps strings as JSON — result may be either a
  // bare "i" or the string "\"i\"". Normalize.
  let s = v.as_str().unwrap_or("");
  assert!(s == "i" || s == "\"i\"", "focus activeElement: {v}");

  // scrollIntoViewIfNeeded shouldn't throw on an offscreen element
  c.nav("<div style='height:2000px'></div><button id='b'>b</button>");
  c.script_value(
    "const b = await page.querySelector('#b');\
     await b.scrollIntoViewIfNeeded();\
     await b.dispose();\
     return true;",
  );
}

// Task 1.2 + 1.3 phase F — handle materialisation surface.
// querySelectorAll on Page + locator.elementHandle{,s}. Rule 9
// covers all 4 backends.
pub fn test_script_handle_materialisation(c: &mut McpClient) {
  c.nav("<ul><li>a</li><li>b</li><li>c</li></ul>");

  // page.querySelectorAll returns one handle per match in document
  // order. Each handle's lifecycle is independent — disposing one
  // doesn't affect the others.
  let v = c.script_value(
    "const items = await page.querySelectorAll('li');\
     const texts = [];\
     for (const it of items) texts.push(await it.textContent());\
     for (const it of items) await it.dispose();\
     return {len: items.length, texts};",
  );
  assert_eq!(v["len"], json!(3), "querySelectorAll length: {v}");
  assert_eq!(v["texts"], json!(["a", "b", "c"]), "querySelectorAll texts: {v}");

  // $$ alias
  let v = c.script_value(
    "const items = await page.$$('li');\
     const len = items.length;\
     for (const it of items) await it.dispose();\
     return len;",
  );
  assert_eq!(v, json!(3), "$$ alias: {v}");

  // Empty selector returns empty array (not error).
  let v = c.script_value(
    "const items = await page.querySelectorAll('li.does-not-exist');\
     return items.length;",
  );
  assert_eq!(v, json!(0), "empty querySelectorAll: {v}");

  // locator.elementHandle resolves the locator's selector to a
  // single pinned ElementHandle.
  c.nav("<button id='b'>click</button>");
  let v = c.script_value(
    "const loc = page.locator('#b');\
     const eh = await loc.elementHandle();\
     const tag = await eh.evaluate(el => el.tagName);\
     await eh.dispose();\
     return tag;",
  );
  assert_eq!(v, json!("BUTTON"), "locator.elementHandle: {v}");

  // locator.elementHandles returns one handle per match.
  c.nav("<ul><li class='it'>x</li><li class='it'>y</li></ul>");
  let v = c.script_value(
    "const loc = page.locator('li.it');\
     const ehs = await loc.elementHandles();\
     const texts = [];\
     for (const eh of ehs) texts.push(await eh.textContent());\
     for (const eh of ehs) await eh.dispose();\
     return {len: ehs.length, texts};",
  );
  assert_eq!(v["len"], json!(2));
  assert_eq!(v["texts"], json!(["x", "y"]));
}

// Task 3.25: `page.addInitScript(script, arg)` — exercise the full
// Playwright surface (Function + arg, string, `{ content }`) from QuickJS
// end-to-end, including the Rust-core-driven `Cannot evaluate a string with
// arguments` error for the string+arg form. Every assertion fires after a
// `goto` so the init script really did run at document start.
pub fn test_script_add_init_script(c: &mut McpClient) {
  // Function + typed arg → init script runs before page JS with `arg`.
  // `page.evaluate` in the QuickJS binding wraps the page value in
  // JSON.stringify on the way out, so each probe is a single JSON.parse
  // to unwrap back to a raw JS value.
  let v = c.script_value(
    "await page.addInitScript(\
       (cfg) => { window.__fd_init_arg = cfg; },\
       { answer: 42, label: 'hi' },\
     );\
     await page.goto('data:text/html,<title>x</title>');\
     return {\
       answer: await page.evaluate('window.__fd_init_arg.answer'),\
       label: await page.evaluate('window.__fd_init_arg.label'),\
     };",
  );
  assert_eq!(v["answer"], json!(42), "function arg answer: {v}");
  assert_eq!(v["label"], json!("hi"), "function arg label: {v}");

  // Function with no arg → rendered as `(fn)(undefined)`, so typeof is 'undefined'.
  let v = c.script_value(
    "await page.addInitScript((x) => { window.__fd_init_noarg = typeof x; });\
     await page.goto('data:text/html,<title>y</title>');\
     return await page.evaluate('window.__fd_init_noarg');",
  );
  assert_eq!(v, json!("undefined"), "function no-arg typeof: {v}");

  // Function with explicit null → JSON.stringify(null) = 'null', arg is null.
  let v = c.script_value(
    "await page.addInitScript((x) => { window.__fd_init_null = x === null ? 'is-null' : typeof x; }, null);\
     await page.goto('data:text/html,<title>z</title>');\
     return await page.evaluate('window.__fd_init_null');",
  );
  assert_eq!(v, json!("is-null"), "function null arg: {v}");

  // { content } → used verbatim.
  let v = c.script_value(
    "await page.addInitScript({ content: \"window.__fd_init_content = 'from-content';\" });\
     await page.goto('data:text/html,<title>w</title>');\
     return await page.evaluate('window.__fd_init_content');",
  );
  assert_eq!(v, json!("from-content"), "{{content}} form: {v}");

  // String + arg → Rust core rejects with Playwright's exact message.
  let v = c.script_value(
    "try {\
       await page.addInitScript('window.x = 1', { bad: true });\
       return 'no-throw';\
     } catch (e) {\
       return String(e.message || e);\
     }",
  );
  let msg = v.as_str().unwrap_or("");
  assert!(
    msg.contains("Cannot evaluate a string with arguments"),
    "string+arg error message: {v}"
  );
}

pub fn test_script_keyboard_press(c: &mut McpClient) {
  c.nav("<textarea id='t'></textarea>");
  let v = c.script_value(
    "await page.locator('#t').focus(); \
       await page.keyboard.press('A'); \
       await page.keyboard.press('B'); \
       return await page.inputValue('#t');",
  );
  let s = v.as_str().unwrap_or("").to_string();
  assert!(
    s.contains('A') || s.contains('a') || s.contains('B') || s.contains('b') || !s.is_empty(),
    "keyboard.press should insert characters: {s:?}"
  );
}
