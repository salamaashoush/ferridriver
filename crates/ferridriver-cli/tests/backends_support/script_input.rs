#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::single_char_pattern,
  clippy::unwrap_used,
  clippy::expect_used
)]
//! run_script: Page interaction tests, extracted from backends.rs.

use serde_json::json;

use super::client::McpClient;

pub fn test_script_click(c: &mut McpClient) {
  c.nav(
    "<h1 id='h'>Before</h1><button id='btn' onclick=\"document.getElementById('h').textContent='After'\">Go</button>",
  );
  let v = c.script_value("await page.click('#btn'); return await page.textContent('#h');");
  assert_eq!(v, json!("After"), "page.click should trigger onclick: {v}");
}

pub fn test_script_fill(c: &mut McpClient) {
  c.nav("<input id='i' type='text'>");
  let v = c.script_value("await page.fill('#i', 'Alice'); return await page.inputValue('#i');");
  assert_eq!(v, json!("Alice"), "page.fill: {v}");
}

pub fn test_script_fill_form(c: &mut McpClient) {
  c.nav("<input id='a'><input id='b'>");
  let v = c.script_value(
    "await page.fill('#a', 'val1'); \
       await page.fill('#b', 'val2'); \
       return { a: await page.inputValue('#a'), b: await page.inputValue('#b') };",
  );
  assert_eq!(v["a"], json!("val1"));
  assert_eq!(v["b"], json!("val2"));
}

pub fn test_script_type(c: &mut McpClient) {
  c.nav("<input id='i' type='text'>");
  let v = c.script_value(
    "await page.locator('#i').click(); \
       await page.type('#i', 'Bob'); \
       return await page.inputValue('#i');",
  );
  assert_eq!(v, json!("Bob"));
}

pub fn test_script_press(c: &mut McpClient) {
  c.nav("<textarea id='t'></textarea>");
  let v = c.script_value(
    "await page.locator('#t').click(); \
       await page.press('#t', 'Enter'); \
       return (await page.inputValue('#t')).length;",
  );
  let len = v.as_i64().unwrap_or(0);
  assert!(len > 0, "press Enter should insert newline, value length: {len}");
}

pub fn test_script_hover(c: &mut McpClient) {
  c.nav("<div id='d' onmouseenter=\"this.textContent='hovered'\" style='width:100px;height:100px'>hover me</div>");
  let v = c.script_value("await page.locator('#d').hover(); return await page.textContent('#d');");
  assert_eq!(v, json!("hovered"), "hover should trigger mouseenter");
}

pub fn test_script_dblclick(c: &mut McpClient) {
  c.nav("<h1 id='h'>0</h1><button id='b' onclick=\"document.getElementById('h').textContent=Number(document.getElementById('h').textContent)+1\">+</button>");
  let v = c.script_value("await page.dblclick('#b'); return await page.textContent('#h');");
  assert_eq!(v, json!("2"), "dblclick should fire two clicks");
}

pub fn test_script_select_option(c: &mut McpClient) {
  c.nav("<select id='s'><option value='apple'>Apple</option><option value='banana'>Banana</option></select>");
  let v = c.script_value(
    "await page.selectOption('#s', 'banana'); \
       return await page.inputValue('#s');",
  );
  assert_eq!(v, json!("banana"));
}

pub fn test_script_check_uncheck(c: &mut McpClient) {
  c.nav("<input id='c' type='checkbox'>");
  let v = c.script_value(
    "await page.check('#c'); \
       const on = await page.isChecked('#c'); \
       await page.uncheck('#c'); \
       const off = await page.isChecked('#c'); \
       return { on, off };",
  );
  assert_eq!(v["on"], json!(true));
  assert_eq!(v["off"], json!(false));
}

pub fn test_script_scroll(c: &mut McpClient) {
  c.nav("<div style='height:3000px'>tall</div>");
  let v = c.script_value(
    "await page.evaluate('window.scrollBy(0, 500)'); \
       const raw = await page.evaluate('window.scrollY'); \
       return raw;",
  );
  let y = v.as_f64().unwrap_or(0.0);
  assert!(y > 0.0, "scroll should change scrollY: {y}");
}

pub fn test_script_scroll_into_view(c: &mut McpClient) {
  c.nav("<div style='height:3000px'></div><div id='bottom'>bottom</div>");
  let v = c.script_value(
    "await page.locator('#bottom').scrollIntoViewIfNeeded(); \
       const raw = await page.evaluate('window.scrollY'); \
       return raw;",
  );
  let y = v.as_f64().unwrap_or(0.0);
  assert!(y > 100.0, "scroll into view should scroll down: {y}");
}

pub fn test_script_click_offscreen(c: &mut McpClient) {
  c.nav("<div style='height:3000px'></div><button id='b' onclick=\"this.textContent='clicked'\">far</button>");
  let v = c.script_value("await page.click('#b'); return await page.textContent('#b');");
  assert_eq!(v, json!("clicked"), "click should auto-scroll offscreen button");
}

pub fn test_script_dialog_alert(c: &mut McpClient) {
  c.nav("<button id='b' onclick=\"alert('hello')\">Go</button>");
  // Dialogs are auto-dismissed; the click should not hang.
  let v = c.script_value("await page.click('#b'); return 'alive';");
  assert_eq!(v, json!("alive"), "should survive alert dialog");
}

pub fn test_script_fill_dispatches_events(c: &mut McpClient) {
  c.nav("<input id='i' type='text'><div id='r'></div><script>document.getElementById('i').addEventListener('change', function(e) { document.getElementById('r').textContent = 'changed:' + e.target.value; });</script>");
  let v = c.script_value(
    "await page.fill('#i', 'test'); \
       return await page.textContent('#r');",
  );
  assert_eq!(v, json!("changed:test"), "fill should dispatch change event");
}

pub fn register(set: &mut crate::TestSet<'_>) {
  set.run("backends_support::script_input::test_script_click", test_script_click);
  set.run("backends_support::script_input::test_script_fill", test_script_fill);
  set.run(
    "backends_support::script_input::test_script_fill_form",
    test_script_fill_form,
  );
  set.run("backends_support::script_input::test_script_type", test_script_type);
  set.run("backends_support::script_input::test_script_press", test_script_press);
  set.run("backends_support::script_input::test_script_hover", test_script_hover);
  set.run(
    "backends_support::script_input::test_script_dblclick",
    test_script_dblclick,
  );
  set.run(
    "backends_support::script_input::test_script_select_option",
    test_script_select_option,
  );
  set.run(
    "backends_support::script_input::test_script_check_uncheck",
    test_script_check_uncheck,
  );
  set.run("backends_support::script_input::test_script_scroll", test_script_scroll);
  set.run(
    "backends_support::script_input::test_script_scroll_into_view",
    test_script_scroll_into_view,
  );
  set.run(
    "backends_support::script_input::test_script_click_offscreen",
    test_script_click_offscreen,
  );
  set.run(
    "backends_support::script_input::test_script_dialog_alert",
    test_script_dialog_alert,
  );
  set.run(
    "backends_support::script_input::test_script_fill_dispatches_events",
    test_script_fill_dispatches_events,
  );
}
