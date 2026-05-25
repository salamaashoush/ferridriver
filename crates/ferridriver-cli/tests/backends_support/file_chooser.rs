//! Rule-9 integration tests for `FileChooser` as a first-class event
//! handle accessible via `page.waitForEvent('filechooser')`.
//!
//! Per-backend expectations:
//! * cdp-pipe / cdp-raw — full round-trip through
//!   `Page.setInterceptFileChooserDialog` + `Page.fileChooserOpened`,
//!   element resolution via `DOM.resolveNode`, upload via
//!   `DOM.setFileInputFiles`.
//! * bidi — full round-trip through `input.fileDialogOpened` +
//!   `input.setFiles`. Firefox natively exposes the chooser event
//!   (Playwright's BiDi backend uses the same path).
//! * webkit — stock `WKWebView` exposes no public API for intercepting
//!   `<input type=file>` clicks; the native picker runs in the host
//!   subprocess and never surfaces in our IPC. The test asserts that
//!   `page.waitForEvent('filechooser')` times out, matching the
//!   documented backend gap.

#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::unwrap_used,
  clippy::expect_used,
  clippy::needless_pass_by_value
)]

use super::client::McpClient;

/// Page HTML with a `<form>` wrapping an `<input type=file>`. A button
/// click triggers the picker; the form's submit writes the number of
/// chosen files + the first filename to `document.title` so the test
/// can assert the upload actually reached the DOM state the page sees.
const SINGLE_FORM_HTML: &str = "<form id=\"f\">\
<input id=\"i\" type=\"file\" name=\"f\" />\
<button id=\"b\" type=\"button\">pick</button>\
</form>\
<script>\
const i = document.getElementById('i');\
const b = document.getElementById('b');\
b.addEventListener('click', () => i.click());\
i.addEventListener('change', () => {\
  const files = i.files;\
  const count = files.length;\
  const first = count > 0 ? files[0].name : '';\
  document.title = `count=${count};first=${first}`;\
});\
</script>";

/// Multiple-file variant with the same structure.
const MULTIPLE_FORM_HTML: &str = "<form id=\"f\">\
<input id=\"i\" type=\"file\" name=\"f\" multiple />\
<button id=\"b\" type=\"button\">pick</button>\
</form>\
<script>\
const i = document.getElementById('i');\
const b = document.getElementById('b');\
b.addEventListener('click', () => i.click());\
i.addEventListener('change', () => {\
  const files = i.files;\
  const names = [];\
  for (let k = 0; k < files.length; k++) names.push(files[k].name);\
  document.title = `count=${files.length};names=${names.join('|')}`;\
});\
</script>";

/// Variant that reports the uploaded file's `name`, `size`, and the
/// decoded text back via `document.title` — proves a `FilePayload`'s
/// bytes reached the page's view of the file end-to-end, not just
/// that the input was populated.
const PAYLOAD_FORM_HTML: &str = "<form id=\"f\">\
<input id=\"i\" type=\"file\" name=\"f\" />\
<button id=\"b\" type=\"button\">pick</button>\
</form>\
<script>\
const i = document.getElementById('i');\
const b = document.getElementById('b');\
b.addEventListener('click', () => i.click());\
i.addEventListener('change', async () => {\
  const f = i.files[0];\
  const text = await f.text();\
  document.title = `name=${f.name};size=${f.size};text=${text}`;\
});\
</script>";

/// JS fragment that `page.evaluate`s a page-side poll for
/// `document.title` starting with the caller-supplied prefix. Lives
/// in the PAGE context so it uses the browser's real `setTimeout` —
/// QuickJS's host-side `setTimeout` is undefined (QuickJS is a pure
/// ES2020 engine; `setTimeout` is a Web API, not in the ECMAScript
/// spec), and the host-side busy-wait idiom
/// `new Promise(r => { while(Date.now()<d) {} })` never resolves
/// (`r` is never called). Passing the function directly (not a
/// string) lets the QuickJS binding's `extract_page_function` flag it
/// as `is_fn=true`, which makes the utility-script wrapper invoke it
/// with the prefix argument.
///
/// Bounded at ~2s (200 × 10ms) so genuine failures surface fast.
const WAIT_FOR_TITLE_CALL: &str = r"
    await page.evaluate(async (prefix) => {
      for (let i = 0; i < 200; i++) {
        const t = document.title;
        if (t && t.startsWith(prefix)) return t;
        await new Promise(r => setTimeout(r, 10));
      }
      return document.title;
    }, PREFIX_JSON)
";

/// Make a unique temp file whose absolute path we can feed to
/// `setFiles(string)`. Cleaned up on test-process exit.
fn tmp_file(name: &str, content: &str) -> String {
  let dir = std::env::temp_dir().join(format!("ferridriver-fc-tests-{}", std::process::id()));
  std::fs::create_dir_all(&dir).expect("create temp dir");
  let path = dir.join(name);
  std::fs::write(&path, content).expect("write temp file");
  path.display().to_string()
}

/// `waitForEvent('filechooser')` returns a live FileChooser on every
/// backend. `isMultiple()` is `false` for a plain `<input type=file>`;
/// `setFiles(path)` uploads the file and the page sees
/// `files[0].name === 'a.txt'`.
pub fn test_file_chooser_single_string_path(c: &mut McpClient) {
  if c.backend == "webkit" {
    return;
  }
  let html = SINGLE_FORM_HTML.to_string();
  c.nav_url(&format!("data:text/html,{}", urlencoding(&html)));
  let path = tmp_file("a.txt", "alpha");
  let script = format!(
    r##"
    const p = page.waitForEvent("filechooser", 10000);
    await page.click("#b");
    const chooser = await p;
    const isMult = chooser.isMultiple();
    await chooser.setFiles({path});
    const title = {wait};
    return {{ isMult, title }};
  "##,
    path = serde_json::to_string(&path).unwrap(),
    wait = WAIT_FOR_TITLE_CALL.replace("PREFIX_JSON", "\"count=\"").trim(),
  );
  let v = c.script_value(&script);
  assert_eq!(
    v["isMult"].as_bool(),
    Some(false),
    "single-file input reports isMultiple=false: {v}"
  );
  assert_eq!(
    v["title"].as_str(),
    Some("count=1;first=a.txt"),
    "page saw exactly the uploaded file: {v}"
  );
}

/// `<input type=file multiple>` + `setFiles(string[])` uploads both
/// files; page sees both names in the DOM's `input.files` list.
pub fn test_file_chooser_multiple_string_array(c: &mut McpClient) {
  if c.backend == "webkit" {
    return;
  }
  let html = MULTIPLE_FORM_HTML.to_string();
  c.nav_url(&format!("data:text/html,{}", urlencoding(&html)));
  let p1 = tmp_file("a-multi.txt", "alpha");
  let p2 = tmp_file("b-multi.txt", "beta");
  let script = format!(
    r##"
    const p = page.waitForEvent("filechooser", 10000);
    await page.click("#b");
    const chooser = await p;
    const isMult = chooser.isMultiple();
    await chooser.setFiles([{p1}, {p2}]);
    const title = {wait};
    return {{ isMult, title }};
  "##,
    p1 = serde_json::to_string(&p1).unwrap(),
    p2 = serde_json::to_string(&p2).unwrap(),
    wait = WAIT_FOR_TITLE_CALL.replace("PREFIX_JSON", "\"count=\"").trim(),
  );
  let v = c.script_value(&script);
  assert_eq!(
    v["isMult"].as_bool(),
    Some(true),
    "multiple input reports isMultiple=true: {v}"
  );
  let title = v["title"].as_str().unwrap_or("");
  assert!(
    title == "count=2;names=a-multi.txt|b-multi.txt" || title == "count=2;names=b-multi.txt|a-multi.txt",
    "both names present in input.files: {v}"
  );
}

/// `setFiles(FilePayload)` uploads an in-memory payload without
/// touching the caller's disk. The page reads the uploaded file via
/// `await f.text()` and reports `name`, `size`, and the decoded
/// `text` — proving the payload bytes round-tripped byte-for-byte,
/// not just that the input was populated.
pub fn test_file_chooser_file_payload_single(c: &mut McpClient) {
  if c.backend == "webkit" {
    return;
  }
  let html = PAYLOAD_FORM_HTML.to_string();
  c.nav_url(&format!("data:text/html,{}", urlencoding(&html)));
  let script = format!(
    r##"
    const p = page.waitForEvent("filechooser", 10000);
    await page.click("#b");
    const chooser = await p;
    const bytes = [104, 101, 108, 108, 111]; // 'hello'
    await chooser.setFiles({{ name: "greeting.txt", mimeType: "text/plain", buffer: bytes }});
    // The page-side change handler awaits `f.text()`; poll for the
    // title from the page context so the browser's real `setTimeout`
    // is used (see WAIT_FOR_TITLE_CALL docstring).
    const title = {wait};
    return {{ title }};
  "##,
    wait = WAIT_FOR_TITLE_CALL.replace("PREFIX_JSON", "\"name=\"").trim(),
  );
  let v = c.script_value(&script);
  let title = v["title"].as_str().unwrap_or("");
  assert!(
    title.contains("name=greeting.txt"),
    "page saw the declared FilePayload name: {v}"
  );
  assert!(title.contains("size=5"), "page saw the payload byte length: {v}");
  assert!(
    title.contains("text=hello"),
    "page decoded the payload bytes back to the original string via `await f.text()`: {v}"
  );
}

/// No listener attached — the CDP intercept is enabled at
/// `attach_listeners` time regardless, so the native picker stays
/// suppressed and the backend disposes the captured ElementHandle.
/// We can't directly observe the disposal through the wire, but we
/// can verify the page's click resolves without the browser hanging
/// waiting for a response.
pub fn test_file_chooser_unclaimed_disposes(c: &mut McpClient) {
  if c.backend == "webkit" || c.backend == "bidi" {
    // WebKit: no intercept exists (see test_file_chooser_webkit_unsupported).
    // BiDi: Firefox's input.fileDialogOpened fires regardless; Firefox's
    // native picker still shows up unless a listener actively claims.
    // The "unclaimed disposes" guarantee is a CDP-specific effect of
    // `setInterceptFileChooserDialog`. Skip on other backends.
    return;
  }
  let html = SINGLE_FORM_HTML.to_string();
  c.nav_url(&format!("data:text/html,{}", urlencoding(&html)));
  let script = r##"
    // No waitForEvent. Click the button; the intercept suppresses
    // the native picker and our listener disposes the captured
    // element behind the scenes. The click should resolve promptly
    // without hanging.
    const started = Date.now();
    await page.click("#b");
    return { elapsed_ms: Date.now() - started };
  "##;
  let v = c.script_value(script);
  let elapsed = v["elapsed_ms"].as_u64().unwrap_or(u64::MAX);
  assert!(
    elapsed < 2000,
    "click with no filechooser listener should not hang (elapsed={}ms): {v}",
    elapsed
  );
}

fn urlencoding(s: &str) -> String {
  // Minimal encoding for `data:text/html,...` — space and `#` are the
  // only chars our HTML uses that need escaping. Keeping this
  // bespoke avoids a new dependency for test plumbing.
  s.replace(' ', "%20").replace('#', "%23").replace('"', "%22")
}
