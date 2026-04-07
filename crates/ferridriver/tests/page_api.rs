#![allow(
  clippy::too_many_lines,
  clippy::uninlined_format_args,
  clippy::single_char_pattern,
  clippy::redundant_closure_for_method_calls,
  clippy::get_first,
)]
//! Integration tests for the ferridriver Page + Locator API.
//!
//! Tests the library API directly -- one browser, sequential tests.

use ferridriver::Browser;
use ferridriver::backend::BackendKind;
use ferridriver::options::*;

fn data_url(html: &str) -> String {
  format!(
    "data:text/html,{}",
    html
      .bytes()
      .map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
          (b as char).to_string()
        },
        _ => format!("%{:02X}", b),
      })
      .collect::<String>()
  )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn page_api_tests() {
  let browser = Browser::launch(LaunchOptions {
    backend: BackendKind::CdpPipe,
    ..Default::default()
  })
  .await
  .expect("launch browser");
  let page = browser.page().await.expect("get page");

  // ── Navigation ──
  page
    .goto(&data_url("<title>Hello</title><body>World</body>"), None)
    .await
    .unwrap();
  let title = page.title().await.unwrap();
  assert!(title.contains("Hello"), "title: {title}");
  let url = page.url().await.unwrap();
  assert!(url.starts_with("data:"), "url: {url}");

  // ── Evaluate ──
  let val = page.evaluate("1 + 2").await.unwrap();
  assert_eq!(val, Some(serde_json::json!(3)));
  let s = page.evaluate_str("'hello'").await.unwrap();
  assert!(s.contains("hello"), "evaluate_str: {s}");

  // ── Locator click ──
  page
    .goto(
      &data_url("<button id='b' onclick=\"this.textContent='clicked'\">Go</button>"),
      None,
    )
    .await
    .unwrap();
  page.locator("#b").click().await.unwrap();
  let t = page
    .evaluate_str("document.getElementById('b').textContent")
    .await
    .unwrap();
  assert!(t.contains("clicked"), "locator click: {t}");

  // ── Locator fill + input_value ──
  page.goto(&data_url("<input id='i' type='text'>"), None).await.unwrap();
  page.locator("#i").fill("hello").await.unwrap();
  let v = page.locator("#i").input_value().await.unwrap();
  assert!(v.contains("hello"), "fill + input_value: {v}");

  // ── get_by_role ──
  page
    .goto(&data_url("<button>Save</button><button>Cancel</button>"), None)
    .await
    .unwrap();
  let count = page
    .get_by_role("button", &RoleOptions::default())
    .count()
    .await
    .unwrap();
  assert_eq!(count, 2, "get_by_role count");

  // ── get_by_text ──
  page
    .goto(&data_url("<p>Hello World</p><p>Goodbye</p>"), None)
    .await
    .unwrap();
  let count = page
    .get_by_text("Hello", &TextOptions::default())
    .count()
    .await
    .unwrap();
  assert_eq!(count, 1, "get_by_text count");

  // ── get_by_label ──
  page
    .goto(
      &data_url("<label for='e'>Email</label><input id='e' type='email'>"),
      None,
    )
    .await
    .unwrap();
  page
    .get_by_label("Email", &TextOptions::default())
    .fill("a@b.com")
    .await
    .unwrap();
  let v = page.evaluate_str("document.getElementById('e').value").await.unwrap();
  assert!(v.contains("a@b.com"), "get_by_label fill: {v}");

  // ── get_by_test_id ──
  page
    .goto(&data_url("<div data-testid='card'>Content</div>"), None)
    .await
    .unwrap();
  let t = page.get_by_test_id("card").text_content().await.unwrap();
  assert!(t.unwrap_or_default().contains("Content"), "get_by_test_id");

  // ── Locator chaining ──
  page
    .goto(
      &data_url("<div class='a'><span>Inside A</span></div><div class='b'><span>Inside B</span></div>"),
      None,
    )
    .await
    .unwrap();
  let t = page.locator("css=.a").locator("css=span").text_content().await.unwrap();
  assert!(t.unwrap_or_default().contains("Inside A"), "chain");

  // ── first / last / nth ──
  page
    .goto(&data_url("<ul><li>A</li><li>B</li><li>C</li></ul>"), None)
    .await
    .unwrap();
  let first = page.locator("css=li").first().text_content().await.unwrap();
  assert!(first.unwrap_or_default().contains("A"), "first");
  let last = page.locator("css=li").last().text_content().await.unwrap();
  assert!(last.unwrap_or_default().contains("C"), "last");
  let second = page.locator("css=li").nth(1).text_content().await.unwrap();
  assert!(second.unwrap_or_default().contains("B"), "nth(1)");

  // ── Visibility ──
  page
    .goto(
      &data_url("<div id='v'>visible</div><div id='h' style='display:none'>hidden</div>"),
      None,
    )
    .await
    .unwrap();
  assert!(page.locator("#v").is_visible().await.unwrap(), "visible");
  assert!(page.locator("#h").is_hidden().await.unwrap(), "hidden");

  // ── Enabled / disabled ──
  page
    .goto(&data_url("<input id='e'><input id='d' disabled>"), None)
    .await
    .unwrap();
  assert!(page.locator("#e").is_enabled().await.unwrap(), "enabled");
  assert!(page.locator("#d").is_disabled().await.unwrap(), "disabled");

  // ── Checked ──
  page
    .goto(&data_url("<input type='checkbox' id='c'>"), None)
    .await
    .unwrap();
  assert!(!page.locator("#c").is_checked().await.unwrap(), "unchecked");
  page.locator("#c").check().await.unwrap();
  assert!(page.locator("#c").is_checked().await.unwrap(), "checked");
  page.locator("#c").uncheck().await.unwrap();
  assert!(!page.locator("#c").is_checked().await.unwrap(), "unchecked again");

  // ── Editable ──
  page
    .goto(&data_url("<input id='e'><input id='d' disabled>"), None)
    .await
    .unwrap();
  assert!(page.locator("#e").is_editable().await.unwrap(), "editable");
  assert!(
    !page.locator("#d").is_editable().await.unwrap(),
    "disabled not editable"
  );

  // ── innerHTML / innerText ──
  page
    .goto(&data_url("<div id='d'><b>Bold</b> text</div>"), None)
    .await
    .unwrap();
  let inner = page.locator("#d").inner_html().await.unwrap();
  assert!(inner.contains("<b>"), "innerHTML: {inner}");
  let text = page.locator("#d").inner_text().await.unwrap();
  assert!(text.contains("Bold"), "innerText: {text}");

  // ── Content + markdown ──
  page
    .goto(&data_url("<h1>Title</h1><p>Body text</p>"), None)
    .await
    .unwrap();
  let html = page.content().await.unwrap();
  assert!(html.contains("Title"), "content");
  let md = page.markdown().await.unwrap();
  assert!(md.contains("# Title"), "markdown heading: {md}");

  // ── all_text_contents ──
  page
    .goto(&data_url("<ul><li>Alpha</li><li>Beta</li><li>Gamma</li></ul>"), None)
    .await
    .unwrap();
  let texts = page.locator("css=li").all_text_contents().await.unwrap();
  assert_eq!(texts.len(), 3);
  assert!(texts[0].contains("Alpha"));
  assert!(texts[2].contains("Gamma"));

  // ── count ──
  let count = page.locator("css=li").count().await.unwrap();
  assert_eq!(count, 3, "count");

  // ── wait_for_selector ──
  page.goto(&data_url("<div id='d'></div><script>setTimeout(function(){document.getElementById('d').innerHTML='<span id=\"s\">loaded</span>'},200)</script>"), None).await.unwrap();
  page
    .wait_for_selector(
      "#s",
      WaitOptions {
        timeout: Some(5000),
        ..Default::default()
      },
    )
    .await
    .unwrap();

  // ── wait_for_function ──
  page
    .goto(
      &data_url("<script>setTimeout(function(){window.ready=true},200)</script>"),
      None,
    )
    .await
    .unwrap();
  let val = page.wait_for_function("window.ready", Some(5000)).await.unwrap();
  assert_eq!(val, serde_json::json!(true));

  // ── Screenshot ──
  page.goto(&data_url("<h1>Screenshot</h1>"), None).await.unwrap();
  let bytes = page.screenshot(ScreenshotOptions::default()).await.unwrap();
  assert!(bytes.len() > 100, "screenshot bytes");
  assert_eq!(&bytes[0..4], &[0x89, 0x50, 0x4E, 0x47], "PNG magic");

  // ── dblclick ──
  page.goto(&data_url("<h1 id='h'>0</h1><button id='b' onclick=\"document.getElementById('h').textContent=Number(document.getElementById('h').textContent)+1\">+</button>"), None).await.unwrap();
  page.locator("#b").dblclick().await.unwrap();
  let t = page
    .evaluate_str("document.getElementById('h').textContent")
    .await
    .unwrap();
  assert!(t.contains("2"), "dblclick: {t}");

  // ── focus + blur ──
  page.goto(&data_url("<input id='i'>"), None).await.unwrap();
  page.locator("#i").focus().await.unwrap();
  let active = page.evaluate_str("document.activeElement?.id||''").await.unwrap();
  assert!(active.contains("i"), "focus: {active}");
  page.locator("#i").blur().await.unwrap();
  let active = page.evaluate_str("document.activeElement?.tagName||''").await.unwrap();
  assert!(!active.contains("INPUT"), "blur: {active}");

  // ── select_option ──
  page
    .goto(
      &data_url("<select id='s'><option value='a'>Apple</option><option value='b'>Banana</option></select>"),
      None,
    )
    .await
    .unwrap();
  page.locator("#s").select_option("Banana").await.unwrap();
  let v = page.evaluate_str("document.getElementById('s').value").await.unwrap();
  assert!(v.contains("b"), "select_option: {v}");

  // ── Click guard on <select> ──
  let r = page.locator("#s").click().await;
  assert!(r.is_err(), "clicking select should error");

  // ── filter ──
  page
    .goto(&data_url("<div><p>Keep</p></div><div><p>Remove</p></div>"), None)
    .await
    .unwrap();
  let count = page
    .locator("css=div")
    .filter(&FilterOptions {
      has_text: Some("Keep".into()),
      ..Default::default()
    })
    .count()
    .await
    .unwrap();
  assert_eq!(count, 1, "filter has_text");

  // ── Viewport configuration ──
  page
    .goto(
      &data_url("<body><script>document.title=window.innerWidth+'x'+window.innerHeight</script></body>"),
      None,
    )
    .await
    .unwrap();
  let initial = page.title().await.unwrap();
  let parts: Vec<&str> = initial.split('x').collect();
  let initial_w: i64 = parts[0].parse().unwrap_or(0);
  let initial_h: i64 = parts[1].parse().unwrap_or(0);
  assert!(initial_w > 0 && initial_h > 0, "initial viewport: {initial}");

  // Set to 1024x768
  page.set_viewport_size(1024, 768).await.unwrap();
  page
    .goto(
      &data_url("<body><script>document.title=window.innerWidth+'x'+window.innerHeight</script></body>"),
      None,
    )
    .await
    .unwrap();
  let resized = page.title().await.unwrap();
  assert!(resized.contains("1024"), "viewport width should be 1024: {resized}");
  assert!(resized.contains("768"), "viewport height should be 768: {resized}");

  // Set to mobile size
  page.set_viewport_size(375, 812).await.unwrap();
  page
    .goto(
      &data_url("<body><script>document.title=window.innerWidth+'x'+window.innerHeight</script></body>"),
      None,
    )
    .await
    .unwrap();
  let mobile = page.title().await.unwrap();
  assert!(mobile.contains("375"), "mobile width should be 375: {mobile}");

  // Restore
  page.set_viewport_size(initial_w, initial_h).await.unwrap();

  browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_for_ai_tests() {
  use ferridriver::snapshot::SnapshotOptions;

  let browser = Browser::launch(LaunchOptions {
    backend: BackendKind::CdpPipe,
    ..Default::default()
  })
  .await
  .expect("launch browser");
  let page = browser.page().await.expect("get page");

  // ── Basic snapshot ──
  page
    .goto(
      &data_url("<h1>Hello World</h1><button>Submit</button><a href='#'>Link</a>"),
      None,
    )
    .await
    .unwrap();
  let result = page.snapshot_for_ai(SnapshotOptions::default()).await.unwrap();
  assert!(result.full.contains("### Page"), "should have page header");
  assert!(
    result.full.contains("heading"),
    "should contain heading role: {}",
    result.full
  );
  assert!(result.full.contains("Hello World"), "should contain heading text");
  assert!(result.full.contains("button"), "should contain button role");
  assert!(result.full.contains("Submit"), "should contain button text");
  assert!(result.full.contains("link"), "should contain link role");
  assert!(
    result.incremental.is_none(),
    "no incremental on first call without track"
  );
  assert!(!result.ref_map.is_empty(), "ref_map should have entries");

  // ── Page header includes URL and title ──
  page
    .goto(&data_url("<title>Test Title</title><body>Content</body>"), None)
    .await
    .unwrap();
  let result = page.snapshot_for_ai(SnapshotOptions::default()).await.unwrap();
  assert!(
    result.full.contains("Title: Test Title"),
    "should contain page title: {}",
    result.full
  );
  assert!(result.full.contains("URL: data:"), "should contain URL");

  // ── Depth limiting ──
  page
    .goto(
      &data_url("<div><ul><li><a href='#'>Deep Link</a></li></ul></div>"),
      None,
    )
    .await
    .unwrap();
  let deep = page
    .snapshot_for_ai(SnapshotOptions {
      depth: None,
      ..Default::default()
    })
    .await
    .unwrap();
  let shallow = page
    .snapshot_for_ai(SnapshotOptions {
      depth: Some(2),
      ..Default::default()
    })
    .await
    .unwrap();
  // Deep should have more content than shallow
  assert!(
    deep.full.len() >= shallow.full.len(),
    "unlimited depth ({}) should be >= depth=2 ({})",
    deep.full.len(),
    shallow.full.len()
  );

  // ── Incremental tracking: first call ──
  page
    .goto(&data_url("<h1>V1</h1><button>Click</button>"), None)
    .await
    .unwrap();
  let r1 = page
    .snapshot_for_ai(SnapshotOptions {
      track: Some("t1".to_string()),
      ..Default::default()
    })
    .await
    .unwrap();
  assert!(r1.full.contains("V1"), "first call should have V1");
  assert!(
    r1.incremental.is_none(),
    "first call with track should have no incremental"
  );

  // ── Incremental tracking: content changed ──
  page
    .goto(&data_url("<h1>V2</h1><button>Click</button>"), None)
    .await
    .unwrap();
  let r2 = page
    .snapshot_for_ai(SnapshotOptions {
      track: Some("t1".to_string()),
      ..Default::default()
    })
    .await
    .unwrap();
  assert!(r2.full.contains("V2"), "second call should have V2");
  assert!(r2.incremental.is_some(), "should have incremental after change");
  let inc = r2.incremental.unwrap();
  assert!(inc.contains("V2"), "incremental should contain changed heading: {inc}");

  // ── Incremental tracking: no change ──
  let r3 = page
    .snapshot_for_ai(SnapshotOptions {
      track: Some("t1".to_string()),
      ..Default::default()
    })
    .await
    .unwrap();
  assert!(r3.incremental.is_none(), "no incremental when nothing changed");

  // ── Ref map has valid refs ──
  page
    .goto(&data_url("<button id='b1'>Save</button><a href='#'>Help</a>"), None)
    .await
    .unwrap();
  let result = page.snapshot_for_ai(SnapshotOptions::default()).await.unwrap();
  // Refs should appear in the snapshot text as [ref=eN]
  let has_refs = result.full.contains("[ref=");
  assert!(has_refs, "snapshot should contain ref labels: {}", result.full);
  // ref_map should map those refs to backend node IDs
  for (ref_label, node_id) in &result.ref_map {
    assert!(ref_label.starts_with('e'), "ref should start with 'e': {ref_label}");
    assert!(*node_id > 0, "backend node ID should be positive: {node_id}");
  }

  browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn add_init_script_tests() {
  let browser = Browser::launch(LaunchOptions {
    backend: BackendKind::CdpPipe,
    ..Default::default()
  })
  .await
  .expect("launch browser");
  let page = browser.page().await.expect("get page");

  // Add an init script that sets a global variable
  let id = page.add_init_script("window.__test_init = 'injected'").await.unwrap();
  assert!(!id.is_empty(), "should return identifier");

  // Navigate -- the init script should run before page JS
  page
    .goto(
      &data_url("<script>document.title = window.__test_init || 'missing'</script>"),
      None,
    )
    .await
    .unwrap();
  let title = page.title().await.unwrap();
  assert_eq!(
    title, "injected",
    "init script should set window.__test_init before page script runs"
  );

  // Navigate again -- init script persists across navigations
  page
    .goto(
      &data_url("<script>document.title = window.__test_init || 'missing'</script>"),
      None,
    )
    .await
    .unwrap();
  let title = page.title().await.unwrap();
  assert_eq!(title, "injected", "init script should persist across navigations");

  // Multiple init scripts
  page.add_init_script("window.__test_init2 = 'second'").await.unwrap();
  page
    .goto(
      &data_url("<script>document.title = (window.__test_init || '') + ':' + (window.__test_init2 || '')</script>"),
      None,
    )
    .await
    .unwrap();
  let title = page.title().await.unwrap();
  assert_eq!(title, "injected:second", "multiple init scripts should all run");

  // Remove the first init script
  page.remove_init_script(&id).await.unwrap();
  page
    .goto(
      &data_url("<script>document.title = (window.__test_init || 'gone') + ':' + (window.__test_init2 || '')</script>"),
      None,
    )
    .await
    .unwrap();
  let title = page.title().await.unwrap();
  assert_eq!(title, "gone:second", "removed init script should no longer run");

  browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dialog_handling_tests() {
  use ferridriver::events::{DialogAction, PendingDialog};
  use std::sync::Arc;

  let browser = Browser::launch(LaunchOptions {
    backend: BackendKind::CdpPipe,
    ..Default::default()
  })
  .await
  .expect("launch browser");
  let page = browser.page().await.expect("get page");

  // Default behavior: auto-accept alerts
  page
    .goto(
      &data_url("<script>alert('hello'); document.title = 'after_alert'</script>"),
      None,
    )
    .await
    .unwrap();
  let title = page.title().await.unwrap();
  assert_eq!(title, "after_alert", "alert should be auto-accepted");

  // Default behavior: auto-accept confirms
  page
    .goto(
      &data_url("<script>var r = confirm('sure?'); document.title = r ? 'yes' : 'no'</script>"),
      None,
    )
    .await
    .unwrap();
  let title = page.title().await.unwrap();
  assert_eq!(title, "yes", "confirm should be auto-accepted");

  // Default behavior: accept prompts with default value
  page
    .goto(
      &data_url("<script>var r = prompt('name?', 'default'); document.title = r || 'null'</script>"),
      None,
    )
    .await
    .unwrap();
  let title = page.title().await.unwrap();
  assert_eq!(title, "default", "prompt should be accepted with default value");

  // Custom handler: dismiss all
  page
    .set_dialog_handler(Arc::new(|_: &PendingDialog| DialogAction::Dismiss))
    .await;
  page
    .goto(
      &data_url("<script>var r = confirm('sure?'); document.title = r ? 'yes' : 'no'</script>"),
      None,
    )
    .await
    .unwrap();
  let title = page.title().await.unwrap();
  assert_eq!(title, "no", "confirm should be dismissed by custom handler");

  // Custom handler: accept prompt with custom text
  page
    .set_dialog_handler(Arc::new(|dialog: &PendingDialog| {
      if dialog.dialog_type == "prompt" {
        DialogAction::Accept(Some("custom_answer".into()))
      } else {
        DialogAction::Accept(None)
      }
    }))
    .await;
  page
    .goto(
      &data_url("<script>var r = prompt('name?'); document.title = r || 'null'</script>"),
      None,
    )
    .await
    .unwrap();
  let title = page.title().await.unwrap();
  assert_eq!(title, "custom_answer", "prompt should get custom answer from handler");

  browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn add_script_style_tag_tests() {
  let browser = Browser::launch(LaunchOptions {
    backend: BackendKind::CdpPipe,
    ..Default::default()
  })
  .await
  .expect("launch browser");
  let page = browser.page().await.expect("get page");

  // Inline script tag
  page.goto(&data_url("<body></body>"), None).await.unwrap();
  page
    .add_script_tag(None, Some("document.title = 'injected'"), None)
    .await
    .unwrap();
  let title = page.title().await.unwrap();
  assert_eq!(title, "injected", "inline script tag should execute");

  // Inline style tag
  page.goto(&data_url("<div id='box'>text</div>"), None).await.unwrap();
  page.add_style_tag(None, Some("#box { color: red }")).await.unwrap();
  let color = page
    .evaluate_str("getComputedStyle(document.getElementById('box')).color")
    .await
    .unwrap();
  assert_eq!(color, "rgb(255, 0, 0)", "inline style tag should apply: {color}");

  browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expose_function_tests() {
  use std::sync::Arc;

  let browser = Browser::launch(LaunchOptions {
    backend: BackendKind::CdpPipe,
    ..Default::default()
  })
  .await
  .expect("launch browser");
  let page = browser.page().await.expect("get page");

  // Expose a simple function that doubles a number
  page
    .expose_function(
      "double",
      Arc::new(|args| {
        let x = args.first().and_then(|v| v.as_f64()).unwrap_or(0.0);
        serde_json::json!(x * 2.0)
      }),
    )
    .await
    .unwrap();

  // Navigate and call the exposed function from page JS
  page.goto(&data_url("<body></body>"), None).await.unwrap();
  let result = page
    .evaluate_str("(async () => { const r = await window.double(21); return String(r); })()")
    .await
    .unwrap();
  assert_eq!(result, "42", "exposed function should return doubled value: {result}");

  // Expose a function that concatenates strings
  page
    .expose_function(
      "greet",
      Arc::new(|args| {
        let name = args.first().and_then(|v| v.as_str()).unwrap_or("world");
        serde_json::json!(format!("Hello, {}!", name))
      }),
    )
    .await
    .unwrap();

  let result = page
    .evaluate_str("(async () => { return await window.greet('Rust'); })()")
    .await
    .unwrap();
  assert_eq!(result, "Hello, Rust!", "greet function should work: {result}");

  // Exposed function persists across navigations
  page.goto(&data_url("<body></body>"), None).await.unwrap();
  let result = page
    .evaluate_str("(async () => { return String(await window.double(5)); })()")
    .await
    .unwrap();
  assert_eq!(
    result, "10",
    "exposed function should persist across navigations: {result}"
  );

  // Multiple arguments
  page
    .expose_function(
      "add",
      Arc::new(|args| {
        let a = args.get(0).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let b = args.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0);
        serde_json::json!(a + b)
      }),
    )
    .await
    .unwrap();

  let result = page
    .evaluate_str("(async () => { return String(await window.add(3, 4)); })()")
    .await
    .unwrap();
  assert_eq!(result, "7", "multi-arg function should work: {result}");

  // Remove exposed function
  page.remove_exposed_function("double").await.unwrap();
  let result = page.evaluate_str("typeof window.double").await.unwrap();
  assert_eq!(result, "undefined", "removed function should be gone");

  browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_for_load_state_tests() {
  let browser = Browser::launch(LaunchOptions {
    backend: BackendKind::CdpPipe,
    ..Default::default()
  })
  .await
  .expect("launch browser");
  let page = browser.page().await.expect("get page");

  // "load" state (default)
  page.goto(&data_url("<body>content</body>"), None).await.unwrap();
  page.wait_for_load_state(Some("load")).await.unwrap();
  let state = page.evaluate_str("document.readyState").await.unwrap();
  assert_eq!(state, "complete", "should be complete after load state");

  // "domcontentloaded" state
  page.goto(&data_url("<body>content</body>"), None).await.unwrap();
  page.wait_for_load_state(Some("domcontentloaded")).await.unwrap();
  let state = page.evaluate_str("document.readyState").await.unwrap();
  assert!(
    state == "interactive" || state == "complete",
    "should be at least interactive after domcontentloaded: {state}"
  );

  // None defaults to "load"
  page.goto(&data_url("<body>content</body>"), None).await.unwrap();
  page.wait_for_load_state(None).await.unwrap();
  let state = page.evaluate_str("document.readyState").await.unwrap();
  assert_eq!(state, "complete");

  // "networkidle" -- page with no pending requests
  page.goto(&data_url("<body>idle</body>"), None).await.unwrap();
  page.wait_for_load_state(Some("networkidle")).await.unwrap();
  // If we get here without timeout, networkidle worked

  browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn locator_evaluate_tests() {
  let browser = Browser::launch(LaunchOptions {
    backend: BackendKind::CdpPipe,
    ..Default::default()
  })
  .await
  .expect("launch browser");
  let page = browser.page().await.expect("get page");

  page.goto(&data_url("<ul><li class='item'>Alpha</li><li class='item'>Beta</li><li class='item'>Gamma</li></ul><h1 id='title'>Hello</h1>"), None).await.unwrap();

  // evaluate on single element
  let tag = page.locator("#title").evaluate("el.tagName").await.unwrap();
  assert_eq!(tag, Some(serde_json::json!("H1")));

  let text = page.locator("#title").evaluate("el.textContent").await.unwrap();
  assert_eq!(text, Some(serde_json::json!("Hello")));

  // evaluate_all on multiple elements
  let count = page.locator("css=.item").evaluate_all("elements.length").await.unwrap();
  assert_eq!(count, Some(serde_json::json!(3)));

  let texts = page
    .locator("css=.item")
    .evaluate_all("elements.map(function(e){return e.textContent})")
    .await
    .unwrap();
  assert_eq!(texts, Some(serde_json::json!(["Alpha", "Beta", "Gamma"])));

  // evaluate with computed values
  let rect = page
    .locator("#title")
    .evaluate("({w: el.offsetWidth, h: el.offsetHeight})")
    .await
    .unwrap();
  assert!(rect.is_some());
  let r = rect.unwrap();
  assert!(
    r.get("w").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0,
    "should have width"
  );

  browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn locator_set_checked_tap_select_text() {
  let browser = Browser::launch(LaunchOptions {
    backend: BackendKind::CdpPipe,
    ..Default::default()
  })
  .await
  .expect("launch browser");
  let page = browser.page().await.expect("get page");

  // set_checked
  page
    .goto(
      &data_url("<input id='cb' type='checkbox'><input id='inp' type='text' value='select me'>"),
      None,
    )
    .await
    .unwrap();
  assert!(!page.locator("#cb").is_checked().await.unwrap());
  page.locator("#cb").set_checked(true).await.unwrap();
  assert!(
    page.locator("#cb").is_checked().await.unwrap(),
    "should be checked after set_checked(true)"
  );
  page.locator("#cb").set_checked(false).await.unwrap();
  assert!(
    !page.locator("#cb").is_checked().await.unwrap(),
    "should be unchecked after set_checked(false)"
  );
  page.locator("#cb").set_checked(true).await.unwrap();
  page.locator("#cb").set_checked(true).await.unwrap(); // idempotent
  assert!(
    page.locator("#cb").is_checked().await.unwrap(),
    "double set_checked(true) should still be checked"
  );

  // select_text
  page.locator("#inp").select_text().await.unwrap();
  let selected = page.evaluate_str("window.getSelection().toString()").await.unwrap();
  assert_eq!(selected, "select me", "select_text should select all text in input");

  // tap -- listens for both touchend (Chrome) and pointerup with touch type (WebKit fallback)
  page.goto(&data_url("<button id='btn'>tap me</button><script>var b=document.getElementById('btn');b.addEventListener('touchend',function(){this.textContent='tapped'});b.addEventListener('pointerup',function(e){if(e.pointerType==='touch')this.textContent='tapped'})</script>"), None).await.unwrap();
  page.locator("#btn").tap().await.unwrap();
  let text = page.locator("#btn").text_content().await.unwrap();
  assert_eq!(
    text.unwrap_or_default(),
    "tapped",
    "tap should fire touch/pointer events"
  );

  browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn storage_state_tests() {
  let browser = Browser::launch(LaunchOptions {
    backend: BackendKind::CdpPipe,
    ..Default::default()
  })
  .await
  .expect("launch browser");
  let page = browser.page().await.expect("get page");

  page.goto(&data_url("<body>storage</body>"), None).await.unwrap();

  // Save storage state (returns correct structure)
  let state = page.storage_state().await.unwrap();
  assert!(state.get("cookies").is_some(), "state should have cookies key");
  assert!(state.get("cookies").unwrap().is_array(), "cookies should be an array");
  assert!(
    state.get("localStorage").is_some(),
    "state should have localStorage key"
  );

  // Test set_storage_state with manual state
  let manual_state = serde_json::json!({
    "cookies": [
      {"name": "test", "value": "val123", "domain": "localhost", "path": "/", "secure": false, "httpOnly": false}
    ],
    "localStorage": {}
  });
  // set_storage_state should not error
  page.set_storage_state(&manual_state).await.unwrap();

  // Save and verify round-trip structure
  let state2 = page.storage_state().await.unwrap();
  assert!(state2.get("cookies").unwrap().is_array());
  assert!(state2.get("localStorage").unwrap().is_object());

  browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn page_close_is_closed_tests() {
  let browser = Browser::launch(LaunchOptions {
    backend: BackendKind::CdpPipe,
    ..Default::default()
  })
  .await
  .expect("launch browser");

  // Open a new page
  let page = browser.new_page_with_url("about:blank").await.unwrap();
  assert!(!page.is_closed(), "new page should not be closed");

  // Close it
  page.close().await.unwrap();
  assert!(page.is_closed(), "page should be closed after close()");

  // Closing again should be idempotent
  page.close().await.unwrap();
  assert!(page.is_closed());

  browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn locator_or_and_tests() {
  let browser = Browser::launch(LaunchOptions {
    backend: BackendKind::CdpPipe,
    ..Default::default()
  })
  .await
  .expect("launch browser");
  let page = browser.page().await.expect("get page");

  page
    .goto(
      &data_url("<button id='a'>Alpha</button><span id='b'>Beta</span><div id='c'>Gamma</div>"),
      None,
    )
    .await
    .unwrap();

  // or() with CSS selectors -- should find elements from either selector
  let combined = page.locator("#a").or(&page.locator("#b"));
  let count = combined.count().await.unwrap();
  assert_eq!(count, 2, "or() should match both selectors: count={count}");

  // first() of or() should find the first matching element
  let text = combined.first().text_content().await.unwrap();
  assert_eq!(text, Some("Alpha".into()), "first() of or() should be Alpha");

  // and() -- intersection (chained selector, narrows scope)
  page.goto(&data_url("<div class='box'><span class='text'>Inside</span></div><div class='other'><span class='text'>Outside</span></div>"), None).await.unwrap();
  let and_loc = page.locator("css=.box").and(&page.locator("css=.text"));
  let text = and_loc.text_content().await.unwrap();
  assert_eq!(text, Some("Inside".into()), "and() should find .text inside .box");

  browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn network_interception_tests() {
  use ferridriver::route::FulfillResponse;
  use std::sync::Arc;

  let browser = Browser::launch(LaunchOptions {
    backend: BackendKind::CdpPipe,
    ..Default::default()
  })
  .await
  .expect("launch browser");
  let page = browser.page().await.expect("get page");

  // 1. Fulfill -- mock a page navigation response
  // Intercept navigations to a test URL and return custom HTML
  page
    .route(
      "**/mock-page",
      Arc::new(|route| {
        route.fulfill(FulfillResponse {
          status: 200,
          body: b"<html><head><title>Mocked</title></head><body>This page is mocked</body></html>".to_vec(),
          content_type: Some("text/html".into()),
          ..Default::default()
        });
      }),
    )
    .await
    .unwrap();

  // Navigate to the mocked URL -- Fetch intercepts the navigation request
  page.goto("http://mock.test/mock-page", None).await.unwrap();
  let title = page.title().await.unwrap();
  assert_eq!(title, "Mocked", "navigated page should show mocked content: {title}");
  let body = page.evaluate_str("document.body.textContent").await.unwrap();
  assert!(
    body.contains("This page is mocked"),
    "body should contain mocked text: {body}"
  );

  // 2. Fulfill with JSON -- mock an API on the now-mocked origin
  page
    .route(
      "**/api/data",
      Arc::new(|route| {
        route.fulfill(FulfillResponse {
          status: 200,
          body: br#"{"mocked":true,"value":42}"#.to_vec(),
          content_type: Some("application/json".into()),
          ..Default::default()
        });
      }),
    )
    .await
    .unwrap();

  let result = page
    .evaluate_str("(async () => { const r = await fetch('/api/data'); return await r.text(); })()")
    .await
    .unwrap();
  assert!(
    result.contains("\"mocked\":true"),
    "API should return mocked JSON: {result}"
  );

  // 3. Abort -- block specific requests
  page
    .route(
      "**/blocked",
      Arc::new(|route| {
        route.abort("blockedbyclient");
      }),
    )
    .await
    .unwrap();

  let result = page
    .evaluate_str(
      "(async () => { try { await fetch('/blocked'); return 'ok'; } catch(e) { return 'error:' + e.message; } })()",
    )
    .await
    .unwrap();
  assert!(result.starts_with("error:"), "blocked request should throw: {result}");

  // 4. Unroute
  page.unroute("**/api/data").await.unwrap();

  browser.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quick_wins_tests() {
  let browser = Browser::launch(LaunchOptions {
    backend: BackendKind::CdpPipe,
    ..Default::default()
  })
  .await
  .expect("launch browser");
  let page = browser.page().await.expect("get page");

  // ── browser.is_connected / version / contexts ──
  assert!(browser.is_connected().await, "browser should be connected");
  assert!(!browser.version().is_empty(), "version should not be empty");
  let contexts = browser.contexts().await;
  assert!(!contexts.is_empty(), "should have at least one context");

  // ── locator.right_click ──
  page.goto(&data_url("<div id='target' oncontextmenu=\"document.title='context_menu';return false\" style='padding:20px'>Right click me</div>"), None).await.unwrap();
  page.locator("#target").right_click().await.unwrap();
  let title = page.title().await.unwrap();
  assert_eq!(title, "context_menu", "right_click should fire contextmenu: {title}");

  // ── locator.is_attached ──
  page.goto(&data_url("<div id='exists'>here</div>"), None).await.unwrap();
  assert!(
    page.locator("#exists").is_attached().await.unwrap(),
    "existing element should be attached"
  );
  assert!(
    !page.locator("#gone").is_attached().await.unwrap(),
    "missing element should not be attached"
  );

  // ── goto with options ──
  page
    .goto(
      &data_url("<title>Opts</title><body>content</body>"),
      Some(GotoOptions {
        wait_until: Some("domcontentloaded".into()),
        timeout: Some(10000),
      }),
    )
    .await
    .unwrap();
  let title = page.title().await.unwrap();
  assert_eq!(title, "Opts", "goto_with_options should work");

  // ── viewport_size ──
  let (w, h) = page.viewport_size().await.unwrap();
  assert!(w > 0 && h > 0, "viewport should have positive dimensions: {w}x{h}");

  // ── page.is_closed after close ──
  let page2 = browser.new_page_with_url("about:blank").await.unwrap();
  assert!(!page2.is_closed());
  page2.close().await.unwrap();
  assert!(page2.is_closed());

  browser.close().await.unwrap();
  assert!(
    !browser.is_connected().await,
    "browser should be disconnected after close"
  );
}
