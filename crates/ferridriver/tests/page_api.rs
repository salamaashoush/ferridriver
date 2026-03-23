//! Integration tests for the ferridriver Page + Locator API.
//!
//! Tests the library API directly -- one browser, sequential tests.

use ferridriver::backend::BackendKind;
use ferridriver::options::*;
use ferridriver::state::ConnectMode;
use ferridriver::Browser;

fn data_url(html: &str) -> String {
  format!(
    "data:text/html,{}",
    html
      .bytes()
      .map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
          (b as char).to_string()
        }
        _ => format!("%{:02X}", b),
      })
      .collect::<String>()
  )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn page_api_tests() {
  let browser = Browser::launch_with(ConnectMode::Launch, BackendKind::CdpWs)
    .await
    .expect("launch browser");
  let page = browser.page().await.expect("get page");

  // ── Navigation ──
  page.goto(&data_url("<title>Hello</title><body>World</body>")).await.unwrap();
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
  page.goto(&data_url("<button id='b' onclick=\"this.textContent='clicked'\">Go</button>")).await.unwrap();
  page.locator("#b").click().await.unwrap();
  let t = page.evaluate_str("document.getElementById('b').textContent").await.unwrap();
  assert!(t.contains("clicked"), "locator click: {t}");

  // ── Locator fill + input_value ──
  page.goto(&data_url("<input id='i' type='text'>")).await.unwrap();
  page.locator("#i").fill("hello").await.unwrap();
  let v = page.locator("#i").input_value().await.unwrap();
  assert!(v.contains("hello"), "fill + input_value: {v}");

  // ── get_by_role ──
  page.goto(&data_url("<button>Save</button><button>Cancel</button>")).await.unwrap();
  let count = page.get_by_role("button", RoleOptions::default()).count().await.unwrap();
  assert_eq!(count, 2, "get_by_role count");

  // ── get_by_text ──
  page.goto(&data_url("<p>Hello World</p><p>Goodbye</p>")).await.unwrap();
  let count = page.get_by_text("Hello", TextOptions::default()).count().await.unwrap();
  assert_eq!(count, 1, "get_by_text count");

  // ── get_by_label ──
  page.goto(&data_url("<label for='e'>Email</label><input id='e' type='email'>")).await.unwrap();
  page.get_by_label("Email", TextOptions::default()).fill("a@b.com").await.unwrap();
  let v = page.evaluate_str("document.getElementById('e').value").await.unwrap();
  assert!(v.contains("a@b.com"), "get_by_label fill: {v}");

  // ── get_by_test_id ──
  page.goto(&data_url("<div data-testid='card'>Content</div>")).await.unwrap();
  let t = page.get_by_test_id("card").text_content().await.unwrap();
  assert!(t.unwrap_or_default().contains("Content"), "get_by_test_id");

  // ── Locator chaining ──
  page.goto(&data_url("<div class='a'><span>Inside A</span></div><div class='b'><span>Inside B</span></div>")).await.unwrap();
  let t = page.locator("css=.a").locator("css=span").text_content().await.unwrap();
  assert!(t.unwrap_or_default().contains("Inside A"), "chain");

  // ── first / last / nth ──
  page.goto(&data_url("<ul><li>A</li><li>B</li><li>C</li></ul>")).await.unwrap();
  let first = page.locator("css=li").first().text_content().await.unwrap();
  assert!(first.unwrap_or_default().contains("A"), "first");
  let last = page.locator("css=li").last().text_content().await.unwrap();
  assert!(last.unwrap_or_default().contains("C"), "last");
  let second = page.locator("css=li").nth(1).text_content().await.unwrap();
  assert!(second.unwrap_or_default().contains("B"), "nth(1)");

  // ── Visibility ──
  page.goto(&data_url("<div id='v'>visible</div><div id='h' style='display:none'>hidden</div>")).await.unwrap();
  assert!(page.locator("#v").is_visible().await.unwrap(), "visible");
  assert!(page.locator("#h").is_hidden().await.unwrap(), "hidden");

  // ── Enabled / disabled ──
  page.goto(&data_url("<input id='e'><input id='d' disabled>")).await.unwrap();
  assert!(page.locator("#e").is_enabled().await.unwrap(), "enabled");
  assert!(page.locator("#d").is_disabled().await.unwrap(), "disabled");

  // ── Checked ──
  page.goto(&data_url("<input type='checkbox' id='c'>")).await.unwrap();
  assert!(!page.locator("#c").is_checked().await.unwrap(), "unchecked");
  page.locator("#c").check().await.unwrap();
  assert!(page.locator("#c").is_checked().await.unwrap(), "checked");
  page.locator("#c").uncheck().await.unwrap();
  assert!(!page.locator("#c").is_checked().await.unwrap(), "unchecked again");

  // ── Editable ──
  page.goto(&data_url("<input id='e'><input id='d' disabled>")).await.unwrap();
  assert!(page.locator("#e").is_editable().await.unwrap(), "editable");
  assert!(!page.locator("#d").is_editable().await.unwrap(), "disabled not editable");

  // ── innerHTML / innerText ──
  page.goto(&data_url("<div id='d'><b>Bold</b> text</div>")).await.unwrap();
  let inner = page.locator("#d").inner_html().await.unwrap();
  assert!(inner.contains("<b>"), "innerHTML: {inner}");
  let text = page.locator("#d").inner_text().await.unwrap();
  assert!(text.contains("Bold"), "innerText: {text}");

  // ── Content + markdown ──
  page.goto(&data_url("<h1>Title</h1><p>Body text</p>")).await.unwrap();
  let html = page.content().await.unwrap();
  assert!(html.contains("Title"), "content");
  let md = page.markdown().await.unwrap();
  assert!(md.contains("# Title"), "markdown heading: {md}");

  // ── all_text_contents ──
  page.goto(&data_url("<ul><li>Alpha</li><li>Beta</li><li>Gamma</li></ul>")).await.unwrap();
  let texts = page.locator("css=li").all_text_contents().await.unwrap();
  assert_eq!(texts.len(), 3);
  assert!(texts[0].contains("Alpha"));
  assert!(texts[2].contains("Gamma"));

  // ── count ──
  let count = page.locator("css=li").count().await.unwrap();
  assert_eq!(count, 3, "count");

  // ── wait_for_selector ──
  page.goto(&data_url("<div id='d'></div><script>setTimeout(function(){document.getElementById('d').innerHTML='<span id=\"s\">loaded</span>'},200)</script>")).await.unwrap();
  page.wait_for_selector("#s", WaitOptions { timeout: Some(5000), ..Default::default() }).await.unwrap();

  // ── wait_for_function ──
  page.goto(&data_url("<script>setTimeout(function(){window.ready=true},200)</script>")).await.unwrap();
  let val = page.wait_for_function("window.ready", Some(5000)).await.unwrap();
  assert_eq!(val, serde_json::json!(true));

  // ── Screenshot ──
  page.goto(&data_url("<h1>Screenshot</h1>")).await.unwrap();
  let bytes = page.screenshot(ScreenshotOptions::default()).await.unwrap();
  assert!(bytes.len() > 100, "screenshot bytes");
  assert_eq!(&bytes[0..4], &[0x89, 0x50, 0x4E, 0x47], "PNG magic");

  // ── dblclick ──
  page.goto(&data_url("<h1 id='h'>0</h1><button id='b' onclick=\"document.getElementById('h').textContent=Number(document.getElementById('h').textContent)+1\">+</button>")).await.unwrap();
  page.locator("#b").dblclick().await.unwrap();
  let t = page.evaluate_str("document.getElementById('h').textContent").await.unwrap();
  assert!(t.contains("2"), "dblclick: {t}");

  // ── focus + blur ──
  page.goto(&data_url("<input id='i'>")).await.unwrap();
  page.locator("#i").focus().await.unwrap();
  let active = page.evaluate_str("document.activeElement?.id||''").await.unwrap();
  assert!(active.contains("i"), "focus: {active}");
  page.locator("#i").blur().await.unwrap();
  let active = page.evaluate_str("document.activeElement?.tagName||''").await.unwrap();
  assert!(!active.contains("INPUT"), "blur: {active}");

  // ── select_option ──
  page.goto(&data_url("<select id='s'><option value='a'>Apple</option><option value='b'>Banana</option></select>")).await.unwrap();
  page.locator("#s").select_option("Banana").await.unwrap();
  let v = page.evaluate_str("document.getElementById('s').value").await.unwrap();
  assert!(v.contains("b"), "select_option: {v}");

  // ── Click guard on <select> ──
  let r = page.locator("#s").click().await;
  assert!(r.is_err(), "clicking select should error");

  // ── filter ──
  page.goto(&data_url("<div><p>Keep</p></div><div><p>Remove</p></div>")).await.unwrap();
  let count = page.locator("css=div").filter(FilterOptions { has_text: Some("Keep".into()), ..Default::default() }).count().await.unwrap();
  assert_eq!(count, 1, "filter has_text");

  // ── Viewport configuration ──
  page.goto(&data_url("<body><script>document.title=window.innerWidth+'x'+window.innerHeight</script></body>")).await.unwrap();
  let initial = page.title().await.unwrap();
  let parts: Vec<&str> = initial.split('x').collect();
  let initial_w: i64 = parts[0].parse().unwrap_or(0);
  let initial_h: i64 = parts[1].parse().unwrap_or(0);
  assert!(initial_w > 0 && initial_h > 0, "initial viewport: {initial}");

  // Set to 1024x768
  page.set_viewport_size(1024, 768).await.unwrap();
  page.goto(&data_url("<body><script>document.title=window.innerWidth+'x'+window.innerHeight</script></body>")).await.unwrap();
  let resized = page.title().await.unwrap();
  assert!(resized.contains("1024"), "viewport width should be 1024: {resized}");
  assert!(resized.contains("768"), "viewport height should be 768: {resized}");

  // Set to mobile size
  page.set_viewport_size(375, 812).await.unwrap();
  page.goto(&data_url("<body><script>document.title=window.innerWidth+'x'+window.innerHeight</script></body>")).await.unwrap();
  let mobile = page.title().await.unwrap();
  assert!(mobile.contains("375"), "mobile width should be 375: {mobile}");

  // Restore
  page.set_viewport_size(initial_w, initial_h).await.unwrap();

  browser.close().await.unwrap();
}
