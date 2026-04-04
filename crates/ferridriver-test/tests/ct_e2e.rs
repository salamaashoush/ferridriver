//! Component testing integration tests.
//!
//! Tests the ComponentServer serving static files and a browser loading them.
//! Does NOT require Leptos/Dioxus/React installed — uses plain HTML+JS components.

use std::sync::Arc;

use ferridriver_test::ct::server::ComponentServer;

/// Test: ComponentServer serves static files and browser can load them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_component_server_serves_files() {
  // Create a temp dir with a simple HTML file.
  let tmp = std::env::temp_dir().join(format!("ferri_ct_server_{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&tmp);
  std::fs::create_dir_all(&tmp).unwrap();

  std::fs::write(
    tmp.join("index.html"),
    r#"<!DOCTYPE html>
<html><head><title>CT Test</title></head>
<body>
<div id="app">
  <button onclick="this.textContent='clicked'">Click Me</button>
</div>
<script>document.body.setAttribute('data-mounted', 'true');</script>
</body></html>"#,
  )
  .unwrap();

  // Start the server.
  let server = ComponentServer::start(&tmp).await.unwrap();
  let url = server.url();
  assert!(url.starts_with("http://127.0.0.1:"));

  // Launch a browser and navigate.
  let browser = ferridriver::Browser::launch(ferridriver::options::LaunchOptions::default())
    .await
    .unwrap();
  let page = browser.new_page_with_url(&url).await.unwrap();

  // Wait for mount signal.
  page
    .wait_for_selector("[data-mounted]", ferridriver::options::WaitOptions::default())
    .await
    .unwrap();

  // Verify the page loaded.
  let title = page.title().await.unwrap();
  assert_eq!(title, "CT Test");

  // Interact with the component.
  page.locator("button").click().await.unwrap();
  let text = page.locator("button").text_content().await.unwrap().unwrap_or_default();
  assert_eq!(text, "clicked");

  // Cleanup.
  let _ = browser.close().await;
  server.stop().await;
  let _ = std::fs::remove_dir_all(&tmp);
}

/// Test: ComponentServer serves WASM HTML wrapper.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_wasm_html_wrapper_structure() {
  let html = ferridriver_test::ct::server::wasm_html_wrapper("my_component.js");
  assert!(html.contains("my_component.js"));
  assert!(html.contains("data-mounted"));
  assert!(html.contains("<div id=\"app\">"));
  assert!(html.contains("type=\"module\""));
}

/// Test: Vite HTML wrapper structure.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_vite_html_wrapper_structure() {
  let html = ferridriver_test::ct::server::vite_html_wrapper("/src/entry.tsx");
  assert!(html.contains("/src/entry.tsx"));
  assert!(html.contains("<div id=\"app\">"));
  assert!(html.contains("type=\"module\""));
}

/// Test: Vite entry generation for each framework.
#[test]
fn test_vite_entry_generation() {
  use ferridriver_test::ct::vite::{generate_entry, JsFramework};

  let react_entry = generate_entry(JsFramework::React, "./Counter.tsx", Some(r#"{ "initial": 5 }"#));
  assert!(react_entry.contains("createRoot"));
  assert!(react_entry.contains("Counter.tsx"));
  assert!(react_entry.contains("\"initial\": 5"));

  let vue_entry = generate_entry(JsFramework::Vue, "./Counter.vue", None);
  assert!(vue_entry.contains("createApp"));
  assert!(vue_entry.contains("Counter.vue"));

  let svelte_entry = generate_entry(JsFramework::Svelte, "./Counter.svelte", None);
  assert!(svelte_entry.contains("new Component"));
  assert!(svelte_entry.contains("Counter.svelte"));

  let solid_entry = generate_entry(JsFramework::Solid, "./Counter.tsx", None);
  assert!(solid_entry.contains("solid-js/web"));
  assert!(solid_entry.contains("Counter.tsx"));
}

/// Test: ComponentServer with a JS "component" (plain JS that mounts DOM).
/// This simulates what a Vite-built component would do.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_js_component_mount_and_interact() {
  let tmp = std::env::temp_dir().join(format!("ferri_ct_js_{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&tmp);
  std::fs::create_dir_all(&tmp).unwrap();

  // A "component" that renders a counter with increment button.
  std::fs::write(
    tmp.join("index.html"),
    r#"<!DOCTYPE html>
<html><head><title>Counter Component</title></head>
<body>
<div id="app"></div>
<script type="module">
  // Simulates what a framework's mount() would do.
  const app = document.getElementById('app');
  let count = 0;
  function render() {
    app.innerHTML = `<span id="count">${count}</span><button id="inc" onclick="window.__inc()">+</button>`;
  }
  window.__inc = () => { count++; render(); };
  render();
  document.body.setAttribute('data-mounted', 'true');
</script>
</body></html>"#,
  )
  .unwrap();

  let server = ComponentServer::start(&tmp).await.unwrap();
  let browser = ferridriver::Browser::launch(ferridriver::options::LaunchOptions::default())
    .await
    .unwrap();
  let page = browser.new_page_with_url(&server.url()).await.unwrap();

  page.wait_for_selector("[data-mounted]", ferridriver::options::WaitOptions::default()).await.unwrap();

  // Verify initial state.
  let count = page.locator("#count").text_content().await.unwrap().unwrap_or_default();
  assert_eq!(count, "0");

  // Click increment 3 times.
  for _ in 0..3 {
    page.locator("#inc").click().await.unwrap();
  }

  // Verify final state.
  let count = page.locator("#count").text_content().await.unwrap().unwrap_or_default();
  assert_eq!(count, "3");

  let _ = browser.close().await;
  server.stop().await;
  let _ = std::fs::remove_dir_all(&tmp);
}
