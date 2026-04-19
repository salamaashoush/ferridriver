//! Per-option integration tests for the remaining §1.5 methods whose
//! full `LocatorFooOptions` surface had signature-only coverage.
//!
//! Each test drives the QuickJS `run_script` binding through the MCP
//! client so it exercises the whole stack: Rust core → backend →
//! QuickJS binding → injected wrappers. The Rule-9 expectation is that
//! every option has a page-visible effect that ONLY occurs when the
//! option took effect — not just that the call didn't error.
//!
//! Tests run on all four backends (`cdp-pipe`, `cdp-raw`, `bidi`,
//! `webkit`). Backend-specific quirks are handled inline; we never
//! `if backend == "..."` skip an assertion unless the backend genuinely
//! cannot perform the operation (and returns a typed `Unsupported`).
//!
//! Timeout coverage lives in `tests/backends.rs::test_script_action_timeout`
//! and exercises dblclick / press / type against a missing selector on
//! every backend; we don't repeat it here.
#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::single_char_pattern,
  clippy::cast_precision_loss,
  clippy::unwrap_used,
  clippy::expect_used,
  clippy::needless_pass_by_value
)]

use super::client::McpClient;
use serde_json::json;

/// `locator.dblclick(opts)` — full `DblClickOptions` surface. Probes
/// every field except `timeout` (covered separately in
/// `test_script_action_timeout`) by setting up a page-side listener
/// that records a different data-attribute for each dispatched side
/// effect.
pub fn test_script_dblclick_options(c: &mut McpClient) {
  // 1. Baseline: ondblclick handler fires on a plain dblclick().
  //    Proves the click_count=2 lowering actually produces a DOM
  //    `dblclick` event (not just two disconnected clicks).
  c.nav(
    "<button id='b'>b</button><div id='out'>no</div>\
     <script>document.getElementById('b').addEventListener('dblclick',()=>document.getElementById('out').textContent='yes')</script>",
  );
  let v = c.script_value(
    "await page.locator('#b').dblclick();\
     return await page.evaluate('document.getElementById(\"out\").textContent');",
  );
  assert_eq!(v, json!("yes"), "dblclick() should fire ondblclick: {v}");

  // 2. modifiers:['Shift'] — dblclick event carries shiftKey.
  c.nav(
    "<button id='b'>b</button><div id='out'>no</div>\
     <script>document.getElementById('b').addEventListener('dblclick',e=>document.getElementById('out').textContent=e.shiftKey?'shift':'none')</script>",
  );
  let v = c.script_value(
    "await page.locator('#b').dblclick({ modifiers: ['Shift'] });\
     return await page.evaluate('document.getElementById(\"out\").textContent');",
  );
  assert_eq!(v, json!("shift"), "dblclick modifiers:['Shift'] sets shiftKey: {v}");

  // 3. position:{x:15,y:25} — the dblclick event fires at the offset
  //    (not the element centre). Use a wide div so centre and offset
  //    are distinguishable at the pixel level.
  c.nav(
    "<div id='b' style='width:200px;height:100px;background:#ccc'></div><div id='out'>none</div>\
     <script>document.getElementById('b').addEventListener('dblclick',e=>{var r=e.currentTarget.getBoundingClientRect();document.getElementById('out').textContent=(Math.round(e.clientX-r.left))+','+(Math.round(e.clientY-r.top))})</script>",
  );
  let v = c.script_value(
    "await page.locator('#b').dblclick({ position: { x: 15, y: 25 } });\
     return await page.evaluate('document.getElementById(\"out\").textContent');",
  );
  assert_eq!(v, json!("15,25"), "dblclick position offsets the event coords: {v}");

  // 4. delay:120 — with a per-click delay, each mousedown→mouseup pair
  //    must hold the button for ≥ 80ms (conservative floor). Record
  //    the first down→up gap only; second pair uses the same delay.
  c.nav(
    "<button id='b'>b</button><div id='out'>0</div>\
     <script>\
       let downAt = 0;\
       let gap = null;\
       const b = document.getElementById('b');\
       b.addEventListener('mousedown', () => { downAt = Date.now(); });\
       b.addEventListener('mouseup', () => { \
         if (gap === null) { gap = Date.now() - downAt; document.getElementById('out').textContent = String(gap); } \
       });\
     </script>",
  );
  let v = c.script_value(
    "await page.locator('#b').dblclick({ delay: 120 });\
     return await page.evaluate('document.getElementById(\"out\").textContent');",
  );
  let ms = v.as_str().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
  assert!(ms >= 80, "dblclick delay:120 held mousedown ≥ 80ms; got {ms} ({v})");

  // 5. trial:true — skips the entire click dispatch; ondblclick never
  //    fires but modifier keydown still does (matches Playwright).
  c.nav(
    "<button id='b'>b</button><div id='dbl'>no</div><div id='kd'>none</div>\
     <script>\
       document.getElementById('b').addEventListener('dblclick',()=>document.getElementById('dbl').textContent='yes');\
       document.addEventListener('keydown',e=>{if(e.key==='Shift')document.getElementById('kd').textContent='shift'});\
     </script>",
  );
  let v = c.script_value(
    "await page.locator('#b').dblclick({ trial: true, modifiers: ['Shift'] });\
     return {\
       dbl: await page.evaluate('document.getElementById(\"dbl\").textContent'),\
       kd: await page.evaluate('document.getElementById(\"kd\").textContent'),\
     };",
  );
  assert_eq!(v["dbl"], json!("no"), "dblclick trial:true skips dispatch: {v}");
  assert_eq!(
    v["kd"],
    json!("shift"),
    "dblclick trial:true still presses modifier: {v}"
  );

  // 6. button:'right' — a right-dblclick emits two `contextmenu` events
  //    with `event.button === 2`. Record both count and button so we
  //    can prove both fields took effect.
  c.nav(
    "<button id='b' oncontextmenu='event.preventDefault()'>b</button><div id='count'>0</div><div id='btn'>-1</div>\
     <script>\
       const b = document.getElementById('b');\
       const cnt = document.getElementById('count');\
       const btn = document.getElementById('btn');\
       b.addEventListener('contextmenu', e => { \
         cnt.textContent = String(parseInt(cnt.textContent,10) + 1); \
         btn.textContent = String(e.button); \
       });\
     </script>",
  );
  let v = c.script_value(
    "await page.locator('#b').dblclick({ button: 'right' });\
     return {\
       count: await page.evaluate('document.getElementById(\"count\").textContent'),\
       btn: await page.evaluate('document.getElementById(\"btn\").textContent'),\
     };",
  );
  // Every backend must emit at least one contextmenu event; CDP + BiDi
  // produce two (one per click of the pair), WebKit coalesces occasionally
  // — allow ≥ 1 to keep the test deterministic cross-backend while still
  // proving button:'right' took effect.
  let count = v["count"].as_str().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
  assert!(count >= 1, "dblclick button:right should fire ≥ 1 contextmenu: {v}");
  assert_eq!(v["btn"], json!("2"), "contextmenu should report button 2: {v}");
}

/// `locator.press(key, opts)` — full `PressOptions` surface. Probes
/// `delay` via a keydown→keyup wall-clock gap and `no_wait_after`
/// via a call that completes without blocking on the page.
pub fn test_script_press_options(c: &mut McpClient) {
  // 1. delay:120 — pressing A with delay should produce a keydown→keyup
  //    gap of at least 80ms on every backend. Record the gap via
  //    `performance.now()` so we capture sub-ms precision.
  c.nav(
    "<input id='i'><div id='out'>0</div>\
     <script>\
       let downAt = 0;\
       const i = document.getElementById('i');\
       i.addEventListener('keydown', () => { downAt = performance.now(); });\
       i.addEventListener('keyup', () => { \
         document.getElementById('out').textContent = String(Math.round(performance.now() - downAt)); \
       });\
     </script>",
  );
  let v = c.script_value(
    "await page.locator('#i').click();\
     await page.locator('#i').press('A', { delay: 120 });\
     return await page.evaluate('document.getElementById(\"out\").textContent');",
  );
  let ms = v.as_str().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
  assert!(ms >= 80, "press delay:120 held key ≥ 80ms; got {ms} ({v})");

  // 2. delay:0 (default) — the same measurement should be near-zero.
  //    Proves that `delay` actually changed the dispatch path and
  //    wasn't a coincidence of backend scheduler granularity.
  c.nav(
    "<input id='i'><div id='out'>0</div>\
     <script>\
       let downAt = 0;\
       const i = document.getElementById('i');\
       i.addEventListener('keydown', () => { downAt = performance.now(); });\
       i.addEventListener('keyup', () => { \
         document.getElementById('out').textContent = String(Math.round(performance.now() - downAt)); \
       });\
     </script>",
  );
  let v = c.script_value(
    "await page.locator('#i').click();\
     await page.locator('#i').press('B');\
     return await page.evaluate('document.getElementById(\"out\").textContent');",
  );
  let ms = v.as_str().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
  assert!(
    ms < 80,
    "press without delay should complete in <80ms; got {ms} ({v}) — suggests delay defaulted non-zero"
  );

  // 3. no_wait_after:true — call returns promptly (< 2s wall-clock).
  //    We can't observe the event-loop distinction directly; the
  //    smoke test is that the option is accepted and the call still
  //    completes in bounded time.
  c.nav("<input id='i'>");
  let v = c.script_value(
    "await page.locator('#i').click();\
     const t0 = Date.now();\
     await page.locator('#i').press('C', { noWaitAfter: true });\
     return Date.now() - t0;",
  );
  let elapsed = v.as_i64().unwrap_or(99_999);
  assert!(
    elapsed < 2_000,
    "press noWaitAfter:true should return promptly; got {elapsed}ms"
  );
}

/// `locator.type(text, opts)` — full `TypeOptions` surface. Probes
/// `delay` via per-character gap measurement.
pub fn test_script_type_options(c: &mut McpClient) {
  // 1. delay:50 over 3 chars should produce at least 2 inter-stroke
  //    gaps each ≥ ~35ms (conservative floor; actual ≈ 50ms minus
  //    scheduler jitter). Record each keydown timestamp as a JSON
  //    array in a single data attribute so we can read it back in one
  //    evaluate call. `autofocus` is unreliable across navigations in
  //    a shared page session, so click the input first.
  c.nav(
    "<input id='i'><div id='marks'>[]</div>\
     <script>\
       const marks = [];\
       document.getElementById('i').addEventListener('keydown', () => { \
         marks.push(performance.now()); \
         document.getElementById('marks').textContent = JSON.stringify(marks); \
       });\
     </script>",
  );
  let v = c.script_value(
    "await page.locator('#i').click();\
     await page.locator('#i').type('abc', { delay: 50 });\
     return await page.evaluate('document.getElementById(\"marks\").textContent');",
  );
  // `page.evaluate` returns a JSON-stringified string; parse it.
  let raw = v.as_str().unwrap_or("[]");
  let marks: Vec<f64> = serde_json::from_str(raw).unwrap_or_default();
  assert_eq!(
    marks.len(),
    3,
    "type('abc', delay:50) should fire 3 keydown events: {v}"
  );
  let g1 = marks[1] - marks[0];
  let g2 = marks[2] - marks[1];
  let min_gap = g1.min(g2);
  assert!(
    min_gap >= 35.0,
    "type delay:50 should hold ≥ 35ms between keystrokes; got g1={g1}ms g2={g2}ms ({v})"
  );

  // 2. Final input value is 'abc' — proves the keys actually typed
  //    into the focused input (not just fired events).
  let after = c.script_value("return await page.inputValue('#i');");
  assert_eq!(after, json!("abc"), "type('abc') should fill the input: {after}");

  // 3. delay:0 (default) — three strokes complete well under the
  //    150ms floor that `delay:50` would require (3 × 50ms).
  c.nav("<input id='i'>");
  let v = c.script_value(
    "await page.locator('#i').click();\
     const t0 = Date.now();\
     await page.locator('#i').type('xyz');\
     return Date.now() - t0;",
  );
  let elapsed = v.as_i64().unwrap_or(99_999);
  assert!(
    elapsed < 1_000,
    "type() without delay should complete in <1s; got {elapsed}ms — suggests delay defaulted non-zero"
  );
  let after = c.script_value("return await page.inputValue('#i');");
  assert_eq!(after, json!("xyz"), "type('xyz') should fill the input: {after}");
}

/// `locator.setInputFiles(files, opts)` — polymorphic
/// `string | string[] | FilePayload | FilePayload[]`. Covers all four
/// forms on every backend; assert on `input.files[i].{name,type,size}`
/// so each form produces a distinct page-visible effect.
pub fn test_script_set_input_files_polymorphism(c: &mut McpClient) {
  // Temp files for the path-based forms. tempfile crate is already a
  // ferridriver dep; using a unique filename per form avoids collisions
  // from the shared `tempfile::env::temp_dir()` root across parallel
  // backends.
  let tmp_dir = std::env::temp_dir();
  let path1 = tmp_dir.join(format!("ferridriver_opts_a_{}.txt", std::process::id()));
  let path2 = tmp_dir.join(format!("ferridriver_opts_b_{}.txt", std::process::id()));
  std::fs::write(&path1, b"alpha").unwrap();
  std::fs::write(&path2, b"beta-beta").unwrap();

  // Form 1 — single path string. The injected setInputFiles uploads
  // one file; read back name + size.
  c.nav("<input type='file' id='f'>");
  let v = c.script_value_with_args(
    "await page.locator('#f').setInputFiles(args[0]); \
     return { \
       count: await page.evaluate(\"document.getElementById('f').files.length\"), \
       name: await page.evaluate(\"document.getElementById('f').files[0].name\"), \
       size: await page.evaluate(\"document.getElementById('f').files[0].size\"), \
     };",
    json!([path1.to_str().unwrap()]),
  );
  assert_eq!(v["count"], json!(1), "single path string: file count=1: {v}");
  assert!(
    v["name"]
      .as_str()
      .unwrap_or("")
      .contains(&format!("ferridriver_opts_a_{}", std::process::id())),
    "single path: name matches fixture: {v}"
  );
  assert_eq!(v["size"], json!(5), "single path: size==5 (alpha): {v}");

  // Form 2 — array of path strings. Two files upload in order.
  c.nav("<input type='file' id='f' multiple>");
  let v = c.script_value_with_args(
    "await page.locator('#f').setInputFiles([args[0], args[1]]); \
     return { \
       count: await page.evaluate(\"document.getElementById('f').files.length\"), \
       n0: await page.evaluate(\"document.getElementById('f').files[0].name\"), \
       n1: await page.evaluate(\"document.getElementById('f').files[1].name\"), \
       s0: await page.evaluate(\"document.getElementById('f').files[0].size\"), \
       s1: await page.evaluate(\"document.getElementById('f').files[1].size\"), \
     };",
    json!([path1.to_str().unwrap(), path2.to_str().unwrap()]),
  );
  assert_eq!(v["count"], json!(2), "path array: file count=2: {v}");
  assert_eq!(v["s0"], json!(5), "path array: first size==5 (alpha): {v}");
  assert_eq!(v["s1"], json!(9), "path array: second size==9 (beta-beta): {v}");

  // Form 3 — single in-memory FilePayload. Buffer is an array of
  // small numbers; serde_from_js deserialises Vec<u8> from any of
  // JS Buffer / Uint8Array / array-of-numbers. Verify the bytes
  // reach the page intact by reading back name, type, size.
  c.nav("<input type='file' id='f'>");
  let payload_bytes: Vec<u8> = b"payload-body".to_vec();
  let v = c.script_value_with_args(
    "await page.locator('#f').setInputFiles(args[0]); \
     return { \
       count: await page.evaluate(\"document.getElementById('f').files.length\"), \
       name: await page.evaluate(\"document.getElementById('f').files[0].name\"), \
       type: await page.evaluate(\"document.getElementById('f').files[0].type\"), \
       size: await page.evaluate(\"document.getElementById('f').files[0].size\"), \
     };",
    json!([{ "name": "payload.txt", "mimeType": "text/plain", "buffer": payload_bytes }]),
  );
  assert_eq!(v["count"], json!(1), "single payload: file count=1: {v}");
  assert_eq!(v["name"], json!("payload.txt"), "single payload: name survives: {v}");
  assert_eq!(v["type"], json!("text/plain"), "single payload: mimeType survives: {v}");
  assert_eq!(
    v["size"],
    json!(payload_bytes.len()),
    "single payload: byte count survives: {v}"
  );

  // Form 4 — array of FilePayloads. Mixed names + mimeTypes; two
  // distinct byte counts so the ordering is observable.
  c.nav("<input type='file' id='f' multiple>");
  let a_bytes: Vec<u8> = b"one".to_vec();
  let b_bytes: Vec<u8> = b"twelvebytes!".to_vec();
  let v = c.script_value_with_args(
    "await page.locator('#f').setInputFiles(args[0]); \
     return { \
       count: await page.evaluate(\"document.getElementById('f').files.length\"), \
       n0: await page.evaluate(\"document.getElementById('f').files[0].name\"), \
       t0: await page.evaluate(\"document.getElementById('f').files[0].type\"), \
       s0: await page.evaluate(\"document.getElementById('f').files[0].size\"), \
       n1: await page.evaluate(\"document.getElementById('f').files[1].name\"), \
       t1: await page.evaluate(\"document.getElementById('f').files[1].type\"), \
       s1: await page.evaluate(\"document.getElementById('f').files[1].size\"), \
     };",
    json!([[
      { "name": "a.txt", "mimeType": "text/plain", "buffer": a_bytes },
      { "name": "b.json", "mimeType": "application/json", "buffer": b_bytes },
    ]]),
  );
  assert_eq!(v["count"], json!(2), "payload array: file count=2: {v}");
  assert_eq!(v["n0"], json!("a.txt"), "payload array: first name: {v}");
  assert_eq!(v["t0"], json!("text/plain"), "payload array: first type: {v}");
  assert_eq!(v["s0"], json!(3), "payload array: first size==3: {v}");
  assert_eq!(v["n1"], json!("b.json"), "payload array: second name: {v}");
  assert_eq!(v["t1"], json!("application/json"), "payload array: second type: {v}");
  assert_eq!(v["s1"], json!(12), "payload array: second size==12: {v}");

  // Cleanup.
  let _ = std::fs::remove_file(&path1);
  let _ = std::fs::remove_file(&path2);
}
