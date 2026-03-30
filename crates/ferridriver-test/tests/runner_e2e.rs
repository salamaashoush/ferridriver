//! End-to-end integration test for the ferridriver test runner.
//!
//! Verifies: discovery, fixture injection, worker pool, expect assertions,
//! terminal reporter, and the full run lifecycle.
//!
//! This uses `harness = false` so we can run the test runner directly.

use std::sync::Arc;
use std::time::Duration;

use ferridriver_test::config::{CliOverrides, TestConfig};
use ferridriver_test::fixture::FixturePool;
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

/// Test: basic page navigation and title check using fixtures.
fn make_navigation_test() -> TestCase {
  TestCase {
    id: TestId {
      file: "runner_e2e.rs".into(),
      suite: Some("navigation".into()),
      name: "basic_navigation".into(),
    },
    test_fn: Arc::new(|pool| {
      Box::pin(async move {
        let page: Arc<ferridriver::Page> = pool.get("page").await.map_err(|e| TestFailure {
          message: e,
          stack: None,
          diff: None,
          screenshot: None,
        })?;
        let url = data_url("<title>Test Page</title><body><h1>Hello World</h1></body>");
        page.goto(&url, None).await.map_err(|e| TestFailure {
          message: format!("goto failed: {e}"),
          stack: None,
          diff: None,
          screenshot: None,
        })?;
        let title = page.title().await.map_err(|e| TestFailure {
          message: format!("title failed: {e}"),
          stack: None,
          diff: None,
          screenshot: None,
        })?;
        if !title.contains("Test Page") {
          return Err(TestFailure {
            message: format!("expected title to contain 'Test Page', got '{title}'"),
            stack: None,
            diff: Some(format!("- expected: \"Test Page\"\n+ received: \"{title}\"")),
            screenshot: None,
          });
        }
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

/// Test: locator click and text assertion.
fn make_click_test() -> TestCase {
  TestCase {
    id: TestId {
      file: "runner_e2e.rs".into(),
      suite: Some("interaction".into()),
      name: "click_button".into(),
    },
    test_fn: Arc::new(|pool| {
      Box::pin(async move {
        let page: Arc<ferridriver::Page> = pool.get("page").await.map_err(|e| TestFailure {
          message: e,
          stack: None,
          diff: None,
          screenshot: None,
        })?;
        let url = data_url("<button id='btn' onclick=\"this.textContent='clicked'\">Click Me</button>");
        page.goto(&url, None).await.map_err(|e| TestFailure {
          message: format!("goto failed: {e}"),
          stack: None,
          diff: None,
          screenshot: None,
        })?;
        page.locator("#btn").click().await.map_err(|e| TestFailure {
          message: format!("click failed: {e}"),
          stack: None,
          diff: None,
          screenshot: None,
        })?;
        let text = page
          .locator("#btn")
          .text_content()
          .await
          .map_err(|e| TestFailure {
            message: format!("text_content failed: {e}"),
            stack: None,
            diff: None,
            screenshot: None,
          })?
          .unwrap_or_default();
        if text != "clicked" {
          return Err(TestFailure {
            message: format!("expected button text 'clicked', got '{text}'"),
            stack: None,
            diff: Some(format!("- expected: \"clicked\"\n+ received: \"{text}\"")),
            screenshot: None,
          });
        }
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

/// Test: fill input and read value.
fn make_fill_test() -> TestCase {
  TestCase {
    id: TestId {
      file: "runner_e2e.rs".into(),
      suite: Some("interaction".into()),
      name: "fill_input".into(),
    },
    test_fn: Arc::new(|pool| {
      Box::pin(async move {
        let page: Arc<ferridriver::Page> = pool.get("page").await.map_err(|e| TestFailure {
          message: e,
          stack: None,
          diff: None,
          screenshot: None,
        })?;
        let url = data_url("<input id='inp' type='text' />");
        page.goto(&url, None).await.map_err(|e| TestFailure {
          message: format!("goto failed: {e}"),
          stack: None,
          diff: None,
          screenshot: None,
        })?;
        page
          .locator("#inp")
          .fill("hello world")
          .await
          .map_err(|e| TestFailure {
            message: format!("fill failed: {e}"),
            stack: None,
            diff: None,
            screenshot: None,
          })?;
        let val = page.locator("#inp").input_value().await.map_err(|e| TestFailure {
          message: format!("input_value failed: {e}"),
          stack: None,
          diff: None,
          screenshot: None,
        })?;
        if val != "hello world" {
          return Err(TestFailure {
            message: format!("expected input value 'hello world', got '{val}'"),
            stack: None,
            diff: None,
            screenshot: None,
          });
        }
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

/// Test: expect assertions (auto-retrying).
fn make_expect_test() -> TestCase {
  TestCase {
    id: TestId {
      file: "runner_e2e.rs".into(),
      suite: Some("expect".into()),
      name: "auto_retry_assertions".into(),
    },
    test_fn: Arc::new(|pool| {
      Box::pin(async move {
        let page: Arc<ferridriver::Page> = pool.get("page").await.map_err(|e| TestFailure {
          message: e,
          stack: None,
          diff: None,
          screenshot: None,
        })?;
        let url = data_url(
          "<title>Expect Test</title>\
           <div id='msg'>Initial</div>\
           <button id='btn' onclick=\"setTimeout(() => document.getElementById('msg').textContent = 'Updated', 200)\">Go</button>",
        );
        page.goto(&url, None).await.map_err(|e| TestFailure {
          message: format!("goto failed: {e}"),
          stack: None,
          diff: None,
          screenshot: None,
        })?;

        // Test page title assertion.
        ferridriver_test::expect::expect(&*page)
          .to_have_title("Expect Test")
          .await?;

        // Click button that updates text after 200ms delay.
        page.locator("#btn").click().await.map_err(|e| TestFailure {
          message: format!("click failed: {e}"),
          stack: None,
          diff: None,
          screenshot: None,
        })?;

        // Auto-retry assertion: should poll until text changes.
        ferridriver_test::expect::expect(&page.locator("#msg"))
          .to_have_text("Updated")
          .await?;

        // Test negation: should NOT have old text.
        ferridriver_test::expect::expect(&page.locator("#msg"))
          .not()
          .to_have_text("Initial")
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

/// Test that should be skipped.
fn make_skip_test() -> TestCase {
  TestCase {
    id: TestId {
      file: "runner_e2e.rs".into(),
      suite: None,
      name: "skipped_test".into(),
    },
    test_fn: Arc::new(|_pool| {
      Box::pin(async move {
        Err(TestFailure {
          message: "this should never run".into(),
          stack: None,
          diff: None,
          screenshot: None,
        })
      })
    }),
    fixture_requests: vec![],
    annotations: vec![TestAnnotation::Skip { reason: Some("testing skip".into()) }],
    timeout: None,
    retries: None,
    expected_status: ExpectedStatus::Pass,
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_runner_e2e() {
  // Build the test plan manually (normally done by #[ferritest] + discovery).
  let plan = TestPlan {
    suites: vec![
      TestSuite {
        name: "navigation".into(),
        file: "runner_e2e.rs".into(),
        tests: vec![make_navigation_test()],
        hooks: Hooks::default(),
        annotations: Vec::new(),
        mode: ferridriver_test::model::SuiteMode::default(),
      },
      TestSuite {
        name: "interaction".into(),
        file: "runner_e2e.rs".into(),
        tests: vec![make_click_test(), make_fill_test()],
        hooks: Hooks::default(),
        annotations: Vec::new(),
        mode: ferridriver_test::model::SuiteMode::default(),
      },
      TestSuite {
        name: "expect".into(),
        file: "runner_e2e.rs".into(),
        tests: vec![make_expect_test()],
        hooks: Hooks::default(),
        annotations: Vec::new(),
        mode: ferridriver_test::model::SuiteMode::default(),
      },
      TestSuite {
        name: "skip".into(),
        file: "runner_e2e.rs".into(),
        tests: vec![make_skip_test()],
        hooks: Hooks::default(),
        annotations: Vec::new(),
        mode: ferridriver_test::model::SuiteMode::default(),
      },
    ],
    total_tests: 5,
    shard: None,
  };

  let config = TestConfig {
    workers: 2,
    timeout: 15_000,
    expect_timeout: 5_000,
    ..Default::default()
  };

  let reporters = reporter::create_reporters(&config.reporter, &config.output_dir);
  let overrides = CliOverrides::default();
  let mut runner = TestRunner::new(config, reporters, overrides);

  let exit_code = runner.run(plan).await;
  assert_eq!(exit_code, 0, "test runner should pass all tests (exit code 0)");
}
