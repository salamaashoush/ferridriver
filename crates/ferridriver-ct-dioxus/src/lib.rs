//! Ferridriver component testing adapter for Dioxus.
//!
//! Same architecture as the Leptos adapter:
//! - `dx build` (cached) → serve target/dx/APP/public/ via ComponentServer
//! - Feed tests into ferridriver-test's parallel runner
//! - N workers × N browsers, MPMC dispatch, retry, reporters
//!
//! ```ignore
//! use ferridriver_ct_dioxus::prelude::*;
//!
//! #[component_test]
//! async fn counter_increments(page: Page) -> Result<(), TestFailure> {
//!     page.locator("#inc").click().await?;
//!     expect(&page.locator("#count")).to_have_text("1").await?;
//!     Ok(())
//! }
//!
//! ferridriver_ct_dioxus::main!();
//! ```

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use ferridriver_test::ct::server::ComponentServer;
use ferridriver_test::model::*;
use ferridriver_test::config::TestConfig;
use ferridriver_test::reporter;
use ferridriver_test::runner::TestRunner;

pub use ferridriver_ct_dioxus_macros::component_test;
pub use ferridriver_test;
pub use inventory;

pub mod prelude {
  pub use ferridriver::{Locator, Page};
  pub use ferridriver_test::expect::expect;
  pub use ferridriver_test::model::TestFailure;
  pub use crate::component_test;
}

pub struct ComponentTestRegistration {
  pub name: &'static str,
  pub test_fn: fn(ferridriver::Page) -> Pin<Box<dyn Future<Output = Result<(), TestFailure>> + Send>>,
}

inventory::collect!(ComponentTestRegistration);

#[macro_export]
macro_rules! main {
  () => {
    fn main() {
      $crate::run_harness();
    }
  };
}

pub fn run_harness() {
  let rt = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(4)
    .enable_all()
    .build()
    .expect("failed to build tokio runtime");

  let exit_code = rt.block_on(async { run_inner().await });
  std::process::exit(exit_code);
}

async fn run_inner() -> i32 {
  let registrations: Vec<&ComponentTestRegistration> =
    inventory::iter::<ComponentTestRegistration>.into_iter().collect();

  if registrations.is_empty() {
    println!("\n  0 tests found.\n");
    return 0;
  }

  let project_dir = find_project_dir();

  // Step 1: dx build.
  dx_build(&project_dir).await;

  // Step 2: Find and serve the built output.
  // Dioxus outputs to target/dx/<app-name>/public/
  let dist_dir = find_dx_dist(&project_dir);
  let server = ComponentServer::start(&dist_dir)
    .await
    .expect("failed to start server");
  let url = server.url();
  eprintln!("[ferridriver-ct] Serving {} test(s) at {url}", registrations.len());

  // Step 3: Convert to TestCases.
  let test_cases: Vec<TestCase> = registrations
    .iter()
    .map(|reg| {
      let test_fn_ptr = reg.test_fn;
      let nav_url = url.clone();

      TestCase {
        id: TestId {
          file: "component_test".into(),
          suite: None,
          name: reg.name.to_string(),
        },
        test_fn: Arc::new(move |pool| {
          let nav_url = nav_url.clone();
          Box::pin(async move {
            let page: Arc<ferridriver::Page> = pool.get("page").await.map_err(|e| TestFailure {
              message: e, stack: None, diff: None, screenshot: None,
            })?;
            page.goto(&nav_url, None).await.map_err(|e| TestFailure {
              message: format!("navigate failed: {e}"), stack: None, diff: None, screenshot: None,
            })?;
            let page_owned = ferridriver::Page::new(page.inner().clone());
            test_fn_ptr(page_owned).await
          })
        }),
        fixture_requests: vec!["page".into()],
        annotations: Vec::new(),
        timeout: Some(Duration::from_secs(30)),
        retries: None,
        expected_status: ExpectedStatus::Pass,
      }
    })
    .collect();

  let total = test_cases.len();

  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "component_tests".into(),
      file: "component_test".into(),
      tests: test_cases,
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: SuiteMode::Parallel,
    }],
    total_tests: total,
    shard: None,
  };

  let config = TestConfig {
    workers: {
      let cpus = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(4);
      (cpus / 2).max(1)
    },
    timeout: 30_000,
    ..Default::default()
  };

  let reporters = reporter::create_reporters(&config.reporter, &config.output_dir);
  let mut runner = TestRunner::new(config, reporters, ferridriver_test::CliOverrides::default());
  let exit_code = runner.run(plan).await;

  server.stop().await;
  exit_code
}

async fn dx_build(project_dir: &PathBuf) {
  eprintln!("[ferridriver-ct] dx build...");
  let start = std::time::Instant::now();

  let output = tokio::process::Command::new("dx")
    .args(["build", "--platform", "web"])
    .current_dir(project_dir)
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .output()
    .await
    .expect("failed to run `dx build` — cargo install dioxus-cli");

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    panic!("[ferridriver-ct] dx build failed:\nstdout: {stdout}\nstderr: {stderr}");
  }

  eprintln!("[ferridriver-ct] built in {:.0?}", start.elapsed());
}

/// Find the Dioxus build output directory.
/// dx 0.7 outputs to target/dx/<app-name>/debug/web/public/
/// dx 0.6 outputs to target/dx/<app-name>/public/
fn find_dx_dist(project_dir: &PathBuf) -> PathBuf {
  // dx outputs relative to the workspace root target/, not the crate's own target/.
  // Check both the crate's target/ and walk up to find the workspace target/.
  let mut search_dirs = vec![project_dir.join("target").join("dx")];

  // Walk up to find workspace root (has Cargo.toml with [workspace]).
  let mut dir = project_dir.clone();
  loop {
    let workspace_target = dir.join("target").join("dx");
    if workspace_target.exists() && !search_dirs.contains(&workspace_target) {
      search_dirs.push(workspace_target);
    }
    if !dir.pop() { break; }
  }

  for dx_dir in &search_dirs {
    if dx_dir.exists() {
      for entry in walkdir(dx_dir) {
        if entry.join("index.html").exists() {
          return entry;
        }
      }
    }
  }

  // Fallback.
  let fallbacks = [project_dir.join("dist")];
  for path in &fallbacks {
    if path.join("index.html").exists() {
      return path.clone();
    }
  }

  panic!(
    "[ferridriver-ct] cannot find Dioxus build output. Searched:\n  {}",
    search_dirs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n  ")
  );
}

/// Recursively find directories containing index.html, deepest first.
fn walkdir(dir: &std::path::Path) -> Vec<PathBuf> {
  let mut results = Vec::new();
  if let Ok(entries) = std::fs::read_dir(dir) {
    for entry in entries.flatten() {
      let path = entry.path();
      if path.is_dir() {
        // Check children first (deeper paths preferred).
        results.extend(walkdir(&path));
        if path.join("index.html").exists() {
          results.push(path);
        }
      }
    }
  }
  results
}

fn find_project_dir() -> PathBuf {
  let mut dir = std::env::current_dir().expect("cannot get cwd");
  loop {
    if dir.join("Cargo.toml").exists() { return dir; }
    if !dir.pop() { panic!("cannot find Cargo.toml"); }
  }
}
