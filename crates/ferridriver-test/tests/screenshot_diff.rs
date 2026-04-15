//! Tests for visual screenshot diffing.
//!
//! These tests mutate process-global env vars (`UPDATE_SNAPSHOTS`, `SNAPSHOT_DIR`)
//! so they run serialized behind a mutex.
#![allow(unsafe_code)]

use std::sync::Mutex;

use ferridriver_test::ct::server::ComponentServer;
use ferridriver_test::expect::expect;

static TEST_MUTEX: Mutex<()> = Mutex::new(());

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn screenshot_creates_baseline_then_matches() {
  let _guard = TEST_MUTEX.lock().unwrap();
  let tmp = std::env::temp_dir().join(format!("ferri_ss_{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&tmp);
  std::fs::create_dir_all(&tmp).unwrap();

  // Serve a simple page.
  std::fs::write(
    tmp.join("index.html"),
    r#"<!DOCTYPE html><html><body style="margin:0;padding:20px;background:white">
    <div id="box" style="width:100px;height:100px;background:red"></div>
    </body></html>"#,
  )
  .unwrap();

  let snap_dir = tmp.join("__snapshots__");
  let server = ComponentServer::start(&tmp).await.unwrap();
  let browser = ferridriver::Browser::launch(ferridriver::options::LaunchOptions::default())
    .await
    .unwrap();
  let page = browser.new_page_with_url(&server.url()).await.unwrap();

  // Point snapshots at our temp dir (no cwd change needed).
  unsafe {
    std::env::set_var("UPDATE_SNAPSHOTS", "1");
    std::env::set_var("SNAPSHOT_DIR", snap_dir.as_os_str());
  }

  // First call: creates baseline.
  let result = expect(&page.locator("#box")).to_have_screenshot("red_box").await;
  assert!(result.is_ok(), "first screenshot should create baseline: {result:?}");

  // Verify baseline file exists.
  assert!(snap_dir.join("red_box.png").exists(), "baseline PNG should exist");
  let baseline_size = std::fs::metadata(snap_dir.join("red_box.png")).unwrap().len();
  assert!(
    baseline_size > 100,
    "baseline should be a real PNG, got {baseline_size}B"
  );

  // Second call with same content: should match.
  unsafe {
    std::env::remove_var("UPDATE_SNAPSHOTS");
  }
  let result = expect(&page.locator("#box")).to_have_screenshot("red_box").await;
  assert!(result.is_ok(), "identical screenshot should match: {result:?}");

  unsafe {
    std::env::remove_var("SNAPSHOT_DIR");
  }
  let _ = browser.close().await;
  server.stop().await;
  let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn screenshot_detects_visual_change() {
  let _guard = TEST_MUTEX.lock().unwrap();
  let tmp = std::env::temp_dir().join(format!("ferri_ss_diff_{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&tmp);
  std::fs::create_dir_all(&tmp).unwrap();

  let snap_dir = tmp.join("__snapshots__");

  std::fs::write(
    tmp.join("index.html"),
    r#"<!DOCTYPE html><html><body style="margin:0;padding:20px;background:white">
    <div id="box" style="width:100px;height:100px;background:red"></div>
    </body></html>"#,
  )
  .unwrap();

  let server = ComponentServer::start(&tmp).await.unwrap();
  let browser = ferridriver::Browser::launch(ferridriver::options::LaunchOptions::default())
    .await
    .unwrap();
  let page = browser.new_page_with_url(&server.url()).await.unwrap();

  // Create baseline.
  unsafe {
    std::env::set_var("UPDATE_SNAPSHOTS", "1");
    std::env::set_var("SNAPSHOT_DIR", snap_dir.as_os_str());
  }
  expect(&page.locator("#box"))
    .to_have_screenshot("color_box")
    .await
    .unwrap();
  unsafe {
    std::env::remove_var("UPDATE_SNAPSHOTS");
  }

  // Change the color.
  page
    .evaluate("(() => { document.getElementById('box').style.background = 'blue'; })()")
    .await
    .unwrap();

  // Should fail with pixel diff.
  let result = expect(&page.locator("#box")).to_have_screenshot("color_box").await;
  assert!(result.is_err(), "changed screenshot should fail");

  let err = result.unwrap_err();
  assert!(err.message.contains("mismatch"), "error should mention mismatch");
  assert!(err.message.contains("pixels differ"), "error should report pixel count");
  assert!(err.screenshot.is_some(), "error should attach actual screenshot");

  // Verify diff image was saved.
  assert!(
    snap_dir.join("color_box-diff.png").exists(),
    "diff image should be saved"
  );
  assert!(
    snap_dir.join("color_box-actual.png").exists(),
    "actual image should be saved"
  );

  // Verify diff image has content.
  let diff_size = std::fs::metadata(snap_dir.join("color_box-diff.png")).unwrap().len();
  assert!(diff_size > 100, "diff PNG should be real, got {diff_size}B");

  unsafe {
    std::env::remove_var("SNAPSHOT_DIR");
  }
  let _ = browser.close().await;
  server.stop().await;
  let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn screenshot_size_mismatch_detected() {
  let _guard = TEST_MUTEX.lock().unwrap();
  let tmp = std::env::temp_dir().join(format!("ferri_ss_size_{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&tmp);
  std::fs::create_dir_all(&tmp).unwrap();

  let snap_dir = tmp.join("__snapshots__");

  std::fs::write(
    tmp.join("index.html"),
    r#"<!DOCTYPE html><html><body style="margin:0;padding:20px;background:white">
    <div id="box" style="width:100px;height:100px;background:green"></div>
    </body></html>"#,
  )
  .unwrap();

  let server = ComponentServer::start(&tmp).await.unwrap();
  let browser = ferridriver::Browser::launch(ferridriver::options::LaunchOptions::default())
    .await
    .unwrap();
  let page = browser.new_page_with_url(&server.url()).await.unwrap();

  // Create baseline.
  unsafe {
    std::env::set_var("UPDATE_SNAPSHOTS", "1");
    std::env::set_var("SNAPSHOT_DIR", snap_dir.as_os_str());
  }
  expect(&page.locator("#box"))
    .to_have_screenshot("size_box")
    .await
    .unwrap();
  unsafe {
    std::env::remove_var("UPDATE_SNAPSHOTS");
  }

  // Resize the element.
  page
    .evaluate(
      "(() => { const b = document.getElementById('box'); b.style.width = '200px'; b.style.height = '200px'; })()",
    )
    .await
    .unwrap();

  // Should fail with size mismatch.
  let result = expect(&page.locator("#box")).to_have_screenshot("size_box").await;
  assert!(result.is_err(), "resized screenshot should fail");
  let err = result.unwrap_err();
  assert!(
    err.message.contains("size mismatch"),
    "error should mention size: {}",
    err.message
  );

  unsafe {
    std::env::remove_var("SNAPSHOT_DIR");
  }
  let _ = browser.close().await;
  server.stop().await;
  let _ = std::fs::remove_dir_all(&tmp);
}
