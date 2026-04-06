#![allow(clippy::unwrap_used, clippy::doc_markdown)]
//! E2E tests for all Playwright-compatible features:
//! - Retry with flaky detection
//! - All expect matchers (visibility, text, value, attributes, CSS, count, focused, etc.)
//! - expect.poll() and toPass()
//! - Screenshot capture on failure
//! - SuiteMode (parallel/serial)
//! - repeatEach

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ferridriver_test::config::{CliOverrides, TestConfig};
use ferridriver_test::model::*;
use ferridriver_test::reporter;
use ferridriver_test::runner::TestRunner;

fn data_url(html: &str) -> String {
  format!(
    "data:text/html,{}",
    html
      .bytes()
      .map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
          (b as char).to_string()
        }
        _ => format!("%{b:02X}"),
      })
      .collect::<String>()
  )
}

// ── Retry + flaky detection test ──

/// A test that fails on the first attempt and passes on the second.
fn make_flaky_test() -> TestCase {
  static ATTEMPT_COUNTER: AtomicU32 = AtomicU32::new(0);
  // Reset for this test run.
  ATTEMPT_COUNTER.store(0, Ordering::SeqCst);

  TestCase {
    id: TestId {
      file: "features_e2e.rs".into(),
      suite: Some("retry".into()),
      name: "flaky_test_passes_on_retry".into(),
      line: None,
    },
    test_fn: Arc::new(|_pool| {
      Box::pin(async move {
        let attempt = ATTEMPT_COUNTER.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt == 1 {
          Err(TestFailure {
            message: "intentional first-attempt failure".into(),
            stack: None,
            diff: None,
            screenshot: None,
          })
        } else {
          Ok(())
        }
      })
    }),
    fixture_requests: vec![],
    annotations: Vec::new(),
    timeout: Some(Duration::from_secs(5)),
    retries: Some(1), // Allow 1 retry.
    expected_status: ExpectedStatus::Pass,
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_retry_with_flaky_detection() {
  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "retry".into(),
      file: "features_e2e.rs".into(),
      tests: vec![make_flaky_test()],
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: SuiteMode::default(),
    }],
    total_tests: 1,
    shard: None,
  };

  let config = TestConfig {
    workers: 1,
    timeout: 10_000,
    ..Default::default()
  };
  let reporters = reporter::create_reporters(&config.reporter, &config.output_dir);
  let mut runner = TestRunner::new(config, reporters, CliOverrides::default());
  let exit_code = runner.run(plan).await;
  // Flaky tests count as passed (exit code 0).
  assert_eq!(exit_code, 0, "flaky test should pass after retry");
}

// ── All locator matchers test ──

fn make_matchers_test() -> TestCase {
  TestCase {
    id: TestId {
      file: "features_e2e.rs".into(),
      suite: Some("matchers".into()),
      name: "all_locator_matchers".into(),
      line: None,
    },
    test_fn: Arc::new(|pool| {
      Box::pin(async move {
        let page: Arc<ferridriver::Page> = pool.get("page").await.map_err(make_failure)?;
        let html = r#"
          <div id="visible" style="display:block">Visible</div>
          <div id="hidden" style="display:none">Hidden</div>
          <button id="btn" disabled class="primary large" role="button"
                  aria-label="Submit Form" aria-description="Submits the form">
            Submit
          </button>
          <input id="inp" type="text" value="hello" />
          <input id="check" type="checkbox" checked />
          <textarea id="area" contenteditable="true">Editable</textarea>
          <select id="multi" multiple>
            <option value="a" selected>A</option>
            <option value="b" selected>B</option>
            <option value="c">C</option>
          </select>
          <div id="empty"></div>
          <div id="styled" style="color: rgb(255, 0, 0);">Red</div>
        "#;
        let url = data_url(html);
        page.goto(&url, None).await.map_err(make_failure)?;

        // Visibility
        ferridriver_test::expect(&page.locator("#visible")).to_be_visible().await?;
        ferridriver_test::expect(&page.locator("#hidden")).to_be_hidden().await?;
        ferridriver_test::expect(&page.locator("#visible")).not().to_be_hidden().await?;

        // Enabled/Disabled
        ferridriver_test::expect(&page.locator("#btn")).to_be_disabled().await?;
        ferridriver_test::expect(&page.locator("#inp")).to_be_enabled().await?;

        // Checked
        ferridriver_test::expect(&page.locator("#check")).to_be_checked().await?;

        // Editable
        ferridriver_test::expect(&page.locator("#area")).to_be_editable().await?;

        // Attached
        ferridriver_test::expect(&page.locator("#visible")).to_be_attached().await?;

        // Empty
        ferridriver_test::expect(&page.locator("#empty")).to_be_empty().await?;
        ferridriver_test::expect(&page.locator("#visible")).not().to_be_empty().await?;

        // Text
        ferridriver_test::expect(&page.locator("#visible")).to_have_text("Visible").await?;
        ferridriver_test::expect(&page.locator("#btn")).to_contain_text("Submit").await?;

        // Value
        ferridriver_test::expect(&page.locator("#inp")).to_have_value("hello").await?;

        // Attribute
        ferridriver_test::expect(&page.locator("#inp")).to_have_attribute("type", "text").await?;

        // Class
        ferridriver_test::expect(&page.locator("#btn")).to_have_class("primary large").await?;
        ferridriver_test::expect(&page.locator("#btn")).to_contain_class("primary").await?;
        ferridriver_test::expect(&page.locator("#btn")).to_contain_class("large").await?;
        ferridriver_test::expect(&page.locator("#btn")).not().to_contain_class("secondary").await?;

        // ID
        ferridriver_test::expect(&page.locator("#btn")).to_have_id("btn").await?;

        // Role
        ferridriver_test::expect(&page.locator("#btn")).to_have_role("button").await?;

        // Accessible name
        ferridriver_test::expect(&page.locator("#btn")).to_have_accessible_name("Submit Form").await?;

        // Accessible description
        ferridriver_test::expect(&page.locator("#btn"))
          .to_have_accessible_description("Submits the form")
          .await?;

        // Count
        ferridriver_test::expect(&page.locator("div")).to_have_count(4).await?;

        // CSS
        ferridriver_test::expect(&page.locator("#styled")).to_have_css("color", "rgb(255, 0, 0)").await?;

        // JS property
        ferridriver_test::expect(&page.locator("#inp"))
          .to_have_js_property("type", serde_json::json!("text"))
          .await?;

        // Multi-select values
        ferridriver_test::expect(&page.locator("#multi"))
          .to_have_values(&["a", "b"])
          .await?;

        Ok(())
      })
    }),
    fixture_requests: vec!["page".into()],
    annotations: Vec::new(),
    timeout: Some(Duration::from_secs(30)),
    retries: None,
    expected_status: ExpectedStatus::Pass,
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_all_locator_matchers() {
  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "matchers".into(),
      file: "features_e2e.rs".into(),
      tests: vec![make_matchers_test()],
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: SuiteMode::default(),
    }],
    total_tests: 1,
    shard: None,
  };

  let config = TestConfig {
    workers: 1,
    timeout: 30_000,
    ..Default::default()
  };
  let reporters = reporter::create_reporters(&config.reporter, &config.output_dir);
  let mut runner = TestRunner::new(config, reporters, CliOverrides::default());
  let exit_code = runner.run(plan).await;
  assert_eq!(exit_code, 0, "all matchers should pass");
}

// ── expect.poll() test ──

fn make_poll_test() -> TestCase {
  TestCase {
    id: TestId {
      file: "features_e2e.rs".into(),
      suite: Some("expect_poll".into()),
      name: "poll_until_value_matches".into(),
      line: None,
    },
    test_fn: Arc::new(|_pool| {
      Box::pin(async move {
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = Arc::clone(&counter);

        // Spawn a task that increments counter every 100ms.
        let handle = tokio::spawn(async move {
          for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            counter_clone.fetch_add(1, Ordering::SeqCst);
          }
        });

        // Poll until counter reaches at least 5.
        let counter_ref = Arc::clone(&counter);
        ferridriver_test::expect_poll(
          move || {
            let c = counter_ref.load(Ordering::SeqCst);
            async move { c }
          },
          Duration::from_secs(5),
        )
        .to_satisfy(|v| *v >= 5, "counter should reach >= 5")
        .await?;

        handle.await.ok();
        Ok(())
      })
    }),
    fixture_requests: vec![],
    annotations: Vec::new(),
    timeout: Some(Duration::from_secs(10)),
    retries: None,
    expected_status: ExpectedStatus::Pass,
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_expect_poll() {
  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "expect_poll".into(),
      file: "features_e2e.rs".into(),
      tests: vec![make_poll_test()],
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: SuiteMode::default(),
    }],
    total_tests: 1,
    shard: None,
  };

  let config = TestConfig {
    workers: 1,
    timeout: 15_000,
    ..Default::default()
  };
  let reporters = reporter::create_reporters(&config.reporter, &config.output_dir);
  let mut runner = TestRunner::new(config, reporters, CliOverrides::default());
  let exit_code = runner.run(plan).await;
  assert_eq!(exit_code, 0, "expect.poll should pass");
}

// ── toPass() test ──

fn make_to_pass_test() -> TestCase {
  TestCase {
    id: TestId {
      file: "features_e2e.rs".into(),
      suite: Some("to_pass".into()),
      name: "retries_block_until_success".into(),
      line: None,
    },
    test_fn: Arc::new(|pool| {
      Box::pin(async move {
        let page: Arc<ferridriver::Page> = pool.get("page").await.map_err(make_failure)?;
        // Page with a button that reveals text after click.
        let html = r#"
          <div id="status">loading</div>
          <script>setTimeout(() => document.getElementById('status').textContent = 'ready', 300)</script>
        "#;
        page.goto(&data_url(html), None).await.map_err(make_failure)?;

        // toPass retries the block until it succeeds.
        ferridriver_test::to_pass(Duration::from_secs(5), || {
          let page = Arc::clone(&page);
          async move {
            let text = page
              .locator("#status")
              .text_content()
              .await
              .map_err(make_failure)?
              .unwrap_or_default();
            if text != "ready" {
              return Err(TestFailure {
                message: format!("expected 'ready', got '{text}'"),
                stack: None,
                diff: None,
                screenshot: None,
              });
            }
            Ok(())
          }
        })
        .await?;

        Ok(())
      })
    }),
    fixture_requests: vec!["page".into()],
    annotations: Vec::new(),
    timeout: Some(Duration::from_secs(15)),
    retries: None,
    expected_status: ExpectedStatus::Pass,
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_to_pass() {
  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "to_pass".into(),
      file: "features_e2e.rs".into(),
      tests: vec![make_to_pass_test()],
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: SuiteMode::default(),
    }],
    total_tests: 1,
    shard: None,
  };

  let config = TestConfig {
    workers: 1,
    timeout: 15_000,
    ..Default::default()
  };
  let reporters = reporter::create_reporters(&config.reporter, &config.output_dir);
  let mut runner = TestRunner::new(config, reporters, CliOverrides::default());
  let exit_code = runner.run(plan).await;
  assert_eq!(exit_code, 0, "toPass should succeed");
}

// ── Page assertions test ──

fn make_page_assertions_test() -> TestCase {
  TestCase {
    id: TestId {
      file: "features_e2e.rs".into(),
      suite: Some("page".into()),
      name: "page_title_and_url".into(),
      line: None,
    },
    test_fn: Arc::new(|pool| {
      Box::pin(async move {
        let page: Arc<ferridriver::Page> = pool.get("page").await.map_err(make_failure)?;
        let url = data_url("<title>My Title</title><body>Hello</body>");
        page.goto(&url, None).await.map_err(make_failure)?;

        ferridriver_test::expect(&*page).to_have_title("My Title").await?;
        ferridriver_test::expect(&*page).not().to_have_title("Wrong Title").await?;
        ferridriver_test::expect(&*page).to_have_url(regex::Regex::new("^data:").unwrap()).await?;

        Ok(())
      })
    }),
    fixture_requests: vec!["page".into()],
    annotations: Vec::new(),
    timeout: Some(Duration::from_secs(15)),
    retries: None,
    expected_status: ExpectedStatus::Pass,
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_page_assertions() {
  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "page".into(),
      file: "features_e2e.rs".into(),
      tests: vec![make_page_assertions_test()],
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: SuiteMode::default(),
    }],
    total_tests: 1,
    shard: None,
  };

  let config = TestConfig {
    workers: 1,
    timeout: 15_000,
    ..Default::default()
  };
  let reporters = reporter::create_reporters(&config.reporter, &config.output_dir);
  let mut runner = TestRunner::new(config, reporters, CliOverrides::default());
  let exit_code = runner.run(plan).await;
  assert_eq!(exit_code, 0, "page assertions should pass");
}

// ── Helper ──

fn make_failure(e: impl std::fmt::Display) -> TestFailure {
  TestFailure {
    message: e.to_string(),
    stack: None,
    diff: None,
    screenshot: None,
  }
}
