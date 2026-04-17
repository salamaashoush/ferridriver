#![allow(
  clippy::cast_precision_loss,
  clippy::cast_lossless,
  clippy::uninlined_format_args,
  clippy::doc_markdown,
  clippy::if_same_then_else
)]
//! Component testing integration tests.
//!
//! Tests the ComponentServer, DevServer URL discovery, and mount() flow.
//! Uses plain HTML+JS to simulate what framework adapters would do.

use ferridriver_test::ct::server::ComponentServer;

/// Test: ComponentServer serves static files and browser can interact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_component_server_serves_and_interacts() {
  let tmp = std::env::temp_dir().join(format!("ferri_ct_{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&tmp);
  std::fs::create_dir_all(&tmp).unwrap();

  // A "component" with a counter — simulates what a framework adapter produces.
  std::fs::write(
    tmp.join("index.html"),
    r#"<!DOCTYPE html>
<html><head><title>CT Test</title></head>
<body>
<div id="app"></div>
<script>
// Simulate framework registerSource: defines __ferriMount.
window.__ferriMount = async function(componentRef, rootEl, options) {
  let count = (options && options.props && options.props.initial) || 0;
  function render() {
    rootEl.innerHTML = `<span id="count">${count}</span><button id="inc">+</button><button id="dec">-</button>`;
    rootEl.querySelector('#inc').onclick = () => { count++; render(); };
    rootEl.querySelector('#dec').onclick = () => { count--; render(); };
  }
  render();
};

// Simulate registry + auto-mount for testing.
window.__ferriMount({ id: 'Counter' }, document.getElementById('app'), { props: { initial: 0 } });
</script>
</body></html>"#,
  )
  .unwrap();

  let server = ComponentServer::start(&tmp).await.unwrap();
  let url = server.url();
  assert!(url.starts_with("http://127.0.0.1:"));

  let browser = ferridriver::Browser::launch(ferridriver::options::LaunchOptions::default())
    .await
    .unwrap();
  let page = browser.new_page_with_url(&url).await.unwrap();

  // Verify initial state.
  let count = page
    .locator("#count", None)
    .text_content()
    .await
    .unwrap()
    .unwrap_or_default();
  assert_eq!(count, "0", "initial count should be 0");

  // Click + three times.
  for _ in 0..3 {
    page.locator("#inc", None).click().await.unwrap();
  }
  let count = page
    .locator("#count", None)
    .text_content()
    .await
    .unwrap()
    .unwrap_or_default();
  assert_eq!(count, "3", "count should be 3 after 3 clicks");

  // Click - once.
  page.locator("#dec", None).click().await.unwrap();
  let count = page
    .locator("#count", None)
    .text_content()
    .await
    .unwrap()
    .unwrap_or_default();
  assert_eq!(count, "2", "count should be 2 after decrement");

  let _ = browser.close(None).await;
  server.stop().await;
  let _ = std::fs::remove_dir_all(&tmp);
}

/// Test: mount() via page.evaluate() with the serialization protocol.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_mount_via_evaluate() {
  let tmp = std::env::temp_dir().join(format!("ferri_ct_mount_{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&tmp);
  std::fs::create_dir_all(&tmp).unwrap();

  // Page with __ferriMount — NO auto-mount, just defines the function.
  std::fs::write(
    tmp.join("index.html"),
    r#"<!DOCTYPE html>
<html><head><title>Mount Test</title></head>
<body>
<div id="app">INITIAL</div>
<script>
window.__ferriMount = function(componentRef, rootEl, options) {
  const props = (options && options.props) || {};
  rootEl.innerHTML = '<div id="mounted" data-component="' + componentRef.id + '" data-initial="' + (props.initial || 0) + '">Mounted: ' + componentRef.id + '</div>';
};
</script>
</body></html>"#,
  )
  .unwrap();

  let server = ComponentServer::start(&tmp).await.unwrap();
  let browser = ferridriver::Browser::launch(ferridriver::options::LaunchOptions::default())
    .await
    .unwrap();
  let page = browser.new_page_with_url(&server.url()).await.unwrap();

  // Verify initial state.
  let initial = page
    .locator("#app", None)
    .text_content()
    .await
    .unwrap()
    .unwrap_or_default();
  assert_eq!(initial, "INITIAL", "page should show initial content");

  // Use ct::mount() to mount a component.
  let component = ferridriver_test::ct::ComponentRef {
    id: "MyCounter".into(),
    props: serde_json::json!({ "initial": 42 }),
    children: vec![],
  };
  let options = ferridriver_test::ct::MountOptions {
    props: serde_json::json!({ "initial": 42 }),
    ..Default::default()
  };

  let _locator = ferridriver_test::ct::mount(&page, &server.url(), &component, &options)
    .await
    .unwrap();

  // Verify the component was mounted.
  let text = page
    .locator("#mounted", None)
    .text_content()
    .await
    .unwrap()
    .unwrap_or_default();
  assert!(
    text.contains("MyCounter"),
    "mounted component should contain ID: {text}"
  );

  let _ = browser.close(None).await;
  server.stop().await;
  let _ = std::fs::remove_dir_all(&tmp);
}

/// Test: DevServer config presets produce correct commands and arguments.
#[test]
fn test_devserver_config_presets() {
  use ferridriver_test::ct::devserver;
  let cwd = std::path::Path::new("/tmp/project");

  // Vite: uses bunx if available, falls back to npx
  let vite = devserver::DevServerConfig::vite(cwd);
  assert!(
    vite.cmd == "bunx" || vite.cmd == "npx",
    "vite cmd should be bunx or npx, got: {}",
    vite.cmd
  );
  assert!(
    vite.args.contains(&"vite".to_string()),
    "vite args should contain 'vite'"
  );
  assert_eq!(vite.cwd, cwd);
  assert_eq!(vite.timeout_secs, 30);

  // Trunk: always uses trunk binary
  let trunk = devserver::DevServerConfig::trunk(cwd);
  assert_eq!(trunk.cmd, "trunk");
  assert_eq!(trunk.args, vec!["serve"]);
  assert_eq!(trunk.cwd, cwd);
  assert_eq!(trunk.timeout_secs, 60);

  // Dioxus: always uses dx binary
  let dx = devserver::DevServerConfig::dioxus(cwd);
  assert_eq!(dx.cmd, "dx");
  assert_eq!(dx.args, vec!["serve"]);
  assert_eq!(dx.cwd, cwd);
  assert_eq!(dx.timeout_secs, 60);

  // Cargo-leptos: uses cargo subcommand
  let leptos = devserver::DevServerConfig::cargo_leptos(cwd);
  assert_eq!(leptos.cmd, "cargo");
  assert_eq!(leptos.args, vec!["leptos", "watch"]);
  assert_eq!(leptos.cwd, cwd);
  assert_eq!(leptos.timeout_secs, 120);
}
