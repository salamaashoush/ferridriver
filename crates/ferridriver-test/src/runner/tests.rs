//! Runner unit tests.

use super::projects::{absolute_path, dependents_of, wildcard_matches};
use super::*;

#[cfg(test)]
mod run_stop_tests {
  use super::RunStop;
  use std::time::Duration;

  /// Every project of a run waits on one stop; a request reaches all of
  /// them, including one that started waiting after it.
  #[tokio::test]
  async fn a_stop_reaches_every_project_waiting_on_it() {
    let stop = RunStop::default();
    let waiting: Vec<_> = (0..3)
      .map(|_| {
        let stop = stop.clone();
        tokio::spawn(async move { stop.wait().await })
      })
      .collect();
    tokio::task::yield_now().await;
    stop.request();
    for project in waiting {
      tokio::time::timeout(Duration::from_millis(100), project)
        .await
        .expect("every waiting project wakes")
        .expect("task");
    }
    tokio::time::timeout(Duration::from_millis(100), stop.wait())
      .await
      .expect("a project that waits after the request returns at once");
  }
}

#[cfg(test)]
mod browser_launch_ownership_tests {
  use super::*;
  use axum::{Json, Router, routing};
  use serde_json::json;
  use std::sync::atomic::{AtomicBool, Ordering};
  use std::time::Duration;
  use tokio::sync::Notify;

  #[tokio::test]
  async fn close_waits_for_cancelled_browser_setup_and_deletes_its_session() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let deleted = Arc::new(AtomicBool::new(false));
    let create = {
      let entered = Arc::clone(&entered);
      let release = Arc::clone(&release);
      move || {
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        async move {
          entered.notify_one();
          release.notified().await;
          Json(json!({"value":{"sessionId":"owned","capabilities":{
            "browserName":"safari","browserVersion":"26.2","setWindowRect":false
          }}}))
        }
      }
    };
    let delete = {
      let deleted = Arc::clone(&deleted);
      move || {
        let deleted = Arc::clone(&deleted);
        async move {
          deleted.store(true, Ordering::Release);
          Json(json!({"value":null}))
        }
      }
    };
    let app = Router::new()
      .route("/session", routing::post(create))
      .route(
        "/session/owned/window/handles",
        routing::get(async || Json(json!({"value":[]}))),
      )
      .route("/session/owned", routing::delete(delete));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let handle = Arc::new(BrowserHandle::with_connection(
      LaunchPlan {
        backend: ferridriver::backend::BackendKind::WebDriver,
        kind: ferridriver::options::BrowserKind::Safari,
        ..Default::default()
      },
      ConnectMode::WebDriver {
        endpoint,
        browser_name: "safari".into(),
        protocol: ferridriver::backend::webdriver::WebDriverProtocol::Classic,
        capabilities: None,
        headers: None,
        timeout: Some(2000),
      },
    ));
    let setup = tokio::spawn({
      let handle = Arc::clone(&handle);
      async move { handle.get().await }
    });
    entered.notified().await;
    setup.abort();
    assert!(matches!(setup.await, Err(error) if error.is_cancelled()));
    let mut closing = tokio::spawn(async move { handle.close().await });
    assert!(
      tokio::time::timeout(Duration::from_millis(25), &mut closing)
        .await
        .is_err()
    );
    release.notify_one();
    closing.await.unwrap().unwrap();
    assert!(deleted.load(Ordering::Acquire));
    server.abort();
  }
}

#[cfg(test)]
mod project_plan_tests {
  use std::sync::Arc;

  use rustc_hash::FxHashMap;

  use super::TestRunner;
  use crate::config::{ProjectConfig, TestConfig};
  use crate::model::{ExpectedStatus, Hooks, SuiteMode, TestCase, TestId, TestPlan, TestSuite};

  fn plan_named(suite: &str) -> TestPlan {
    TestPlan {
      repetitions_expanded: false,
      suites: vec![TestSuite {
        serial_scope: None,
        name: suite.to_string(),
        file: format!("{suite}.feature"),
        tests: vec![TestCase {
          metadata: None,
          id: TestId {
            root_path: None,
            repeat_each_index: 0,
            file: format!("{suite}.feature"),
            suite: Some(suite.to_string()),
            name: "a scenario".to_string(),
            line: None,
            column: None,
          },
          test_fn: Arc::new(|_| Box::pin(async { Ok(()) })),
          fixture_requests: Vec::new(),
          annotations: Vec::new(),
          timeout: None,
          retries: None,
          expected_status: ExpectedStatus::Pass,
          use_options: None,
        }],
        hooks: Hooks::default(),
        annotations: Vec::new(),
        mode: SuiteMode::Parallel,
      }],
      total_tests: 1,
      shard: None,
    }
  }

  fn runner_with(projects: Vec<&str>) -> TestRunner {
    TestRunner::new(
      TestConfig {
        projects: projects
          .into_iter()
          .map(|name| ProjectConfig {
            name: name.to_string(),
            ..Default::default()
          })
          .collect(),
        reporter: Vec::new(),
        ..Default::default()
      },
      crate::config::CliOverrides::default(),
    )
  }

  /// One suite per file, each with one test, as discovery hands them.
  fn plan_of_files(files: &[std::path::PathBuf]) -> TestPlan {
    let suites: Vec<TestSuite> = files
      .iter()
      .map(|file| {
        let mut suite = plan_named("suite").suites.remove(0);
        suite.file = file.to_string_lossy().into_owned();
        for test in &mut suite.tests {
          test.id.file.clone_from(&suite.file);
        }
        suite
      })
      .collect();
    TestPlan {
      repetitions_expanded: false,
      total_tests: suites.len(),
      suites,
      shard: None,
    }
  }

  /// A run that is no configured project, as a config without projects
  /// has, has nothing depending on it; one named among projects keeps
  /// the projects that need it.
  #[test]
  fn dependents_of_a_run_outside_the_configured_projects_are_none() {
    let projects = vec![
      ProjectConfig {
        name: "setup".into(),
        ..Default::default()
      },
      ProjectConfig {
        name: "app".into(),
        dependencies: vec!["setup".into()],
        ..Default::default()
      },
    ];
    let dependents = super::dependents_of(&[], &[usize::MAX]);
    assert_eq!(dependents.get(&usize::MAX), Some(&Vec::new()));
    let dependents = super::dependents_of(&projects, &[0, 1, usize::MAX]);
    assert_eq!(dependents.get(&0), Some(&vec![1]));
    assert_eq!(dependents.get(&1), Some(&Vec::new()));
  }

  /// Each project runs only the files its own testMatch and testIgnore
  /// pick, and a file named on the command line still brings the setup
  /// project its project depends on, as Playwright's
  /// `collectProjectsAndTestFiles` does.
  #[test]
  fn projects_run_their_own_files_and_a_named_file_keeps_its_setup() {
    let dir = tempfile::tempdir().expect("temp dir");
    let e2e = dir.path().join("e2e");
    std::fs::create_dir_all(e2e.join("console")).expect("dirs");
    let files: Vec<std::path::PathBuf> = ["portal.spec.ts", "console/owner.setup.ts", "console/dunning.spec.ts"]
      .iter()
      .map(|name| {
        let file = e2e.join(name);
        std::fs::write(&file, "").expect("write spec");
        crate::identity::comparable_path(&file)
      })
      .collect();
    let plan = plan_of_files(&files);
    let projects = vec![
      ProjectConfig {
        name: "chromium".into(),
        test_ignore: Some(vec!["console/**".into()]),
        ..Default::default()
      },
      ProjectConfig {
        name: "setup".into(),
        test_match: Some(vec!["console/owner.setup.ts".into()]),
        ..Default::default()
      },
      ProjectConfig {
        name: "console".into(),
        test_match: Some(vec!["console/**/*.spec.ts".into()]),
        dependencies: vec!["setup".into()],
        ..Default::default()
      },
    ];
    let listed = |overrides: crate::config::CliOverrides| -> Vec<(String, String)> {
      let runner = TestRunner::new(
        TestConfig {
          projects: projects.clone(),
          test_match: vec!["**/*.spec.ts".into()],
          reporter: Vec::new(),
          ..Default::default()
        },
        overrides,
      );
      runner
        .list(&plan)
        .expect("list")
        .into_iter()
        .map(|test| {
          let name = std::path::Path::new(&test.id.file)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
          (test.record.project, name)
        })
        .collect()
    };
    let pairs = |items: &[(&str, &str)]| -> Vec<(String, String)> {
      items.iter().map(|(a, b)| ((*a).to_owned(), (*b).to_owned())).collect()
    };

    assert_eq!(
      listed(crate::config::CliOverrides::default()),
      pairs(&[
        ("chromium", "portal.spec.ts"),
        ("setup", "owner.setup.ts"),
        ("console", "dunning.spec.ts")
      ])
    );
    let dunning = crate::config::CliOverrides {
      test_files: vec![files[2].to_string_lossy().into_owned()],
      ..Default::default()
    };
    assert_eq!(
      listed(dunning),
      pairs(&[("setup", "owner.setup.ts"), ("console", "dunning.spec.ts")])
    );
    let portal = crate::config::CliOverrides {
      test_files: vec!["portal".into()],
      ..Default::default()
    };
    assert_eq!(listed(portal), pairs(&[("chromium", "portal.spec.ts")]));
  }

  /// The run's worker slots serve whichever project has work, as
  /// Playwright's dispatcher does: once a short project finishes, a long
  /// one started beside it takes its slot and runs on every slot the run
  /// has, rather than on the share it was given when both started.
  #[tokio::test]
  async fn a_project_takes_the_slots_another_project_gives_back() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let dir = tempfile::tempdir().expect("temp dir");
    let file = |name: &str| {
      let path = dir.path().join(name);
      std::fs::write(&path, "").expect("write spec");
      crate::identity::comparable_path(&path)
    };
    let mut files = vec![file("short.spec.ts")];
    files.extend((0..8).map(|n| file(&format!("long-{n}.spec.ts"))));
    let mut plan = plan_of_files(&files);
    let running = Arc::new(AtomicUsize::new(0));
    let most = Arc::new(AtomicUsize::new(0));
    for suite in plan.suites.iter_mut().filter(|suite| suite.file.contains("long-")) {
      for test in &mut suite.tests {
        let running = Arc::clone(&running);
        let most = Arc::clone(&most);
        test.test_fn = Arc::new(move |_| {
          let running = Arc::clone(&running);
          let most = Arc::clone(&most);
          Box::pin(async move {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            most.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            running.fetch_sub(1, Ordering::SeqCst);
            Ok(())
          })
        });
      }
    }
    let project = |name: &str, pattern: &str| ProjectConfig {
      name: name.into(),
      test_match: Some(vec![pattern.into()]),
      ..Default::default()
    };
    let mut runner = TestRunner::new(
      TestConfig {
        projects: vec![project("long", "long-*.spec.ts"), project("short", "short.spec.ts")],
        test_match: vec!["**/*.spec.ts".into()],
        workers: Some(2),
        reporter: Vec::new(),
        ..Default::default()
      },
      crate::config::CliOverrides::default(),
    );
    assert_eq!(runner.run(plan).await, super::exit::PASSED);
    assert_eq!(most.load(Ordering::SeqCst), 2);
  }

  /// A run keeps how long each test took, and the next run starts the
  /// longest first, so no long test is left to run alone at the end.
  #[tokio::test]
  async fn the_next_run_starts_the_tests_that_took_longest() {
    use std::sync::Mutex;

    let dir = tempfile::tempdir().expect("temp dir");
    // Far enough apart that a timer ticking every 15.6ms (Windows) or a
    // loaded CI scheduler running parallel test binaries cannot swap two.
    let lengths = [10u64, 460, 160, 310];
    let files: Vec<std::path::PathBuf> = lengths
      .iter()
      .map(|ms| {
        let path = dir.path().join(format!("takes-{ms}.spec.ts"));
        std::fs::write(&path, "").expect("write spec");
        crate::identity::comparable_path(&path)
      })
      .collect();
    let started: Arc<Mutex<Vec<u64>>> = Arc::default();
    let plan = || {
      let mut plan = plan_of_files(&files);
      for (suite, &ms) in plan.suites.iter_mut().zip(&lengths) {
        let started = Arc::clone(&started);
        suite.tests[0].test_fn = Arc::new(move |_| {
          let started = Arc::clone(&started);
          Box::pin(async move {
            started.lock().expect("order").push(ms);
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            Ok(())
          })
        });
      }
      plan
    };
    let run = || {
      TestRunner::new(
        TestConfig {
          config_dir: Some(dir.path().to_path_buf()),
          workers: Some(1),
          reporter: Vec::new(),
          ..Default::default()
        },
        crate::config::CliOverrides::default(),
      )
    };
    assert_eq!(run().run(plan()).await, super::exit::PASSED);
    assert_eq!(
      *started.lock().expect("order"),
      lengths,
      "nothing known yet: plan order"
    );
    started.lock().expect("order").clear();
    assert_eq!(run().run(plan()).await, super::exit::PASSED);
    assert_eq!(*started.lock().expect("order"), [460, 310, 160, 10]);
  }

  /// A project given its own plan runs THAT plan, not the shared one —
  /// the whole point of the store, and a silent fallback to the shared
  /// plan would look like a passing run over the wrong corpus.
  #[test]
  fn a_project_with_its_own_plan_runs_it_instead_of_the_shared_one() {
    let runner = runner_with(vec!["smoke", "regression"]);
    let mut plans = FxHashMap::default();
    plans.insert("smoke".to_string(), plan_named("smoke-only"));
    runner.set_project_plans(plans);

    let shared = plan_named("shared");
    let runs = runner.project_runs();

    let smoke = runs.iter().find(|r| r.name == "smoke").expect("smoke run");
    assert_eq!(smoke.narrow(&shared).suites[0].name, "smoke-only");

    // And a project with no plan of its own still narrows the shared one.
    let regression = runs.iter().find(|r| r.name == "regression").expect("regression run");
    assert_eq!(regression.narrow(&shared).suites[0].name, "shared");
  }
}

#[cfg(test)]
mod project_filter_tests {
  use super::{absolute_path, select_projects, wildcard_matches};
  use crate::config::ProjectConfig;

  fn project(name: &str, default: Option<bool>) -> ProjectConfig {
    ProjectConfig {
      name: name.to_string(),
      default,
      ..Default::default()
    }
  }

  #[test]
  fn wildcards_match_the_whole_name() {
    assert!(wildcard_matches("slow*", "slow"));
    assert!(wildcard_matches("slow*", "slow-nightly"));
    assert!(wildcard_matches("*-nightly", "slow-nightly"));
    assert!(wildcard_matches("s*w*y", "slow-nightly"));
    assert!(!wildcard_matches("slow*", "very-slow"));
    assert!(!wildcard_matches("ab*b", "ab"));
    assert!(!wildcard_matches("*a*a", "a"));
  }

  #[test]
  fn no_filter_selects_default_projects_unless_asked_for_all() {
    let projects = [project("a", None), project("b", Some(false)), project("c", Some(true))];
    assert_eq!(select_projects(&projects, &[], false), Ok(vec![0, 2]));
    assert_eq!(select_projects(&projects, &[], true), Ok(vec![0, 1, 2]));
  }

  #[test]
  fn an_unknown_literal_name_is_an_error_even_beside_a_match() {
    let projects = [project("a", None), project("b", Some(false))];
    let names = ["B".to_string(), "zz".to_string()];
    assert_eq!(
      select_projects(&projects, &names, false),
      Err(r#"Project(s) "zz" not found. Available projects: "a", "b""#.to_string())
    );
  }

  /// A spec discovered outside the working directory reaches
  /// `filter_plan_for_project` as a `../..`-prefixed relative path.
  /// Making it absolute is not enough — the `..` components have to go,
  /// or the `testDir` prefix match fails and every project reports "no
  /// tests matched".
  #[test]
  fn an_out_of_tree_path_normalizes_before_it_is_compared() {
    let cwd = std::env::current_dir().expect("cwd");
    let resolved = absolute_path("../sibling-that-does-not-exist/specs/a.spec.ts");
    assert!(
      !resolved.components().any(|c| c.as_os_str() == ".."),
      "{} still carries `..`",
      resolved.display()
    );
    let expected = cwd
      .parent()
      .expect("cwd has a parent")
      .join("sibling-that-does-not-exist/specs/a.spec.ts");
    assert_eq!(resolved, expected);
    assert!(resolved.starts_with(absolute_path("../sibling-that-does-not-exist/specs")));
  }

  /// The other half: a bundler names a spec through its real path while
  /// `testDir` keeps the symlink the user wrote (`/var` -> `/private/var`
  /// on macOS). Both sides have to resolve the link or the prefix match
  /// fails on a path that exists.
  #[test]
  fn a_symlinked_test_dir_compares_equal_to_the_real_path() {
    let base = std::env::temp_dir().join(format!("ferri-abs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let real = base.join("real/specs");
    std::fs::create_dir_all(&real).expect("create dirs");
    let file = real.join("a.spec.ts");
    std::fs::write(&file, "").expect("write spec");

    let link = base.join("linked");
    #[cfg(unix)]
    std::os::unix::fs::symlink(base.join("real"), &link).expect("symlink");
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(base.join("real"), &link).expect("symlink");

    let via_link = absolute_path(&link.join("specs/a.spec.ts").to_string_lossy());
    let via_real = absolute_path(&file.to_string_lossy());
    assert_eq!(via_link, via_real);
    assert!(via_real.starts_with(absolute_path(&link.join("specs").to_string_lossy())));
    let _ = std::fs::remove_dir_all(&base);
  }
}
