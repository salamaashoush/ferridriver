//! `ferridriver test --ui` — the web UI for `#[ferritest]` harness
//! tests driven through cargo.
//!
//! The CLI hosts the same localhost app `bdd --ui` serves
//! ([`ferridriver_test::ui_server::UiServer`]) but, unlike BDD, the
//! tests live in separate harness binaries that must be recompiled when
//! sources change. So instead of running tests in-process, every cycle
//! spawns `cargo test` with `FERRITEST_UI_SOCK` exported; harness
//! binaries connect back over that unix socket (see
//! [`ferridriver_test::ui_wire`]) and stream a `testList` hello plus
//! reporter events as JSON lines already in the app's wire shape, which
//! this bridge forwards to browser tabs.
//!
//! Two cycle kinds:
//! - **List** (startup, and after a watched file changes): `cargo test
//!   --tests -- --list` with `FERRITEST_LIST=1` — every harness binary
//!   compiles, sends its hello, and exits; libtest binaries in scope
//!   just print their list. Sidebar binaries that stopped helloing are
//!   pruned.
//! - **Run** (UI command): `cargo test --test <bin>` for each harness
//!   binary holding a selected test — never bare `cargo test`, which
//!   would run every unrelated workspace test and doctest per click.
//!   The selection itself travels as `FERRITEST_GREP` (patterns,
//!   single-test anchors) or `FERRITEST_ID_FILE` (exact id lists for
//!   run-file / run-failed, which can be too large for an env var).
//!   Stop kills the child's process group (cargo, harnesses, and their
//!   browsers share it).
//!
//! The run lifecycle (`runStarted` / `runFinished`) is owned here, not
//! by harness binaries: several test binaries can participate in one
//! cycle, so their per-binary run boundaries are swallowed and
//! re-aggregated from the forwarded `testFinished` events.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;

use ferridriver_config::FerridriverConfig;
use ferridriver_test::ui_server::{UiCommand, UiServer, UiState};
use ferridriver_test::ui_wire;

use crate::cli;

/// Sidebar aggregator: each harness binary's hello contributes its
/// suites; the combined list is what tabs render.
#[derive(Default)]
struct Sidebar {
  suites_by_binary: BTreeMap<String, Vec<serde_json::Value>>,
  /// Binaries that helloed during the current cycle — after a clean
  /// list cycle, anything absent here no longer exists and is pruned.
  hellos_this_cycle: BTreeSet<String>,
}

impl Sidebar {
  fn record_hello(&mut self, message: &serde_json::Value) {
    let binary = message
      .get("binary")
      .and_then(|b| b.as_str())
      .unwrap_or("harness")
      .to_string();
    let suites = message
      .get("suites")
      .and_then(|s| s.as_array())
      .cloned()
      .unwrap_or_default();
    if self.hellos_this_cycle.insert(binary.clone()) {
      self.suites_by_binary.insert(binary, suites);
    } else {
      // Second hello under the same key within one cycle: two distinct
      // binaries share a stem (same integration-test name in different
      // packages). Append instead of overwriting the first one's suites.
      self.suites_by_binary.entry(binary).or_default().extend(suites);
    }
  }

  fn prune_missing(&mut self) {
    let seen = std::mem::take(&mut self.hellos_this_cycle);
    self.suites_by_binary.retain(|binary, _| seen.contains(binary));
    self.hellos_this_cycle = seen;
  }

  fn combined(&self) -> serde_json::Value {
    let suites: Vec<serde_json::Value> = self.suites_by_binary.values().flatten().cloned().collect();
    serde_json::json!({ "type": "testList", "suites": suites })
  }

  fn test_ids(&self) -> Vec<String> {
    self
      .suites_by_binary
      .values()
      .flatten()
      .filter_map(|suite| suite.get("tests").and_then(|t| t.as_array()))
      .flatten()
      .filter_map(|test| test.get("id").and_then(|i| i.as_str()).map(str::to_string))
      .collect()
  }

  fn ids_in_file(&self, file: &str) -> Vec<String> {
    self
      .suites_by_binary
      .values()
      .flatten()
      .filter(|suite| suite.get("file").and_then(|f| f.as_str()) == Some(file))
      .filter_map(|suite| suite.get("tests").and_then(|t| t.as_array()))
      .flatten()
      .filter_map(|test| test.get("id").and_then(|i| i.as_str()).map(str::to_string))
      .collect()
  }

  /// Binaries with at least one test whose id matches `pred`, plus the
  /// total number of matching tests — the cycle builds only those
  /// `--test` targets.
  fn binaries_matching(&self, pred: &dyn Fn(&str) -> bool) -> (BTreeSet<String>, usize) {
    let mut binaries = BTreeSet::new();
    let mut matched = 0usize;
    for (binary, suites) in &self.suites_by_binary {
      for suite in suites {
        for test in suite.get("tests").and_then(|t| t.as_array()).into_iter().flatten() {
          let Some(id) = test.get("id").and_then(|i| i.as_str()) else {
            continue;
          };
          if pred(id) {
            binaries.insert(binary.clone());
            matched += 1;
          }
        }
      }
    }
    (binaries, matched)
  }

  /// Binaries contributing at least one suite for `file`.
  fn binaries_with_file(&self, file: &str) -> BTreeSet<String> {
    self
      .suites_by_binary
      .iter()
      .filter(|(_, suites)| {
        suites
          .iter()
          .any(|suite| suite.get("file").and_then(|f| f.as_str()) == Some(file))
      })
      .map(|(binary, _)| binary.clone())
      .collect()
  }
}

enum CycleKind {
  List,
  Run,
}

struct Cycle {
  generation: u64,
  pid: Option<i32>,
  kind: CycleKind,
  started: std::time::Instant,
  passed: usize,
  failed: usize,
  skipped: usize,
  flaky: usize,
}

/// How a run cycle communicates its selection to harness binaries and
/// which cargo `--test` targets it builds.
struct RunScope {
  /// `FERRITEST_GREP` value — small user patterns and single-test
  /// anchors only.
  grep: Option<String>,
  /// Exact test ids, passed via `FERRITEST_ID_FILE` — run-file /
  /// run-failed scopes can span hundreds of ids, and an env-var
  /// alternation that long risks the kernel's env size limit (E2BIG).
  ids: Option<Vec<String>>,
  /// Planned test count for `runStarted.totalTests`. Computed with
  /// `discovery::grep_matcher`, so it matches what the harness selects.
  planned: usize,
  /// Harness binaries containing the selected tests; the cycle passes
  /// each as a cargo `--test` target so unrelated workspace tests and
  /// doctests do not run.
  binaries: BTreeSet<String>,
}

/// Resolve a run command into a [`RunScope`]. `None` means the command
/// is a no-op (nothing matches / no recorded failures / unknown test).
fn plan_run(command: &UiCommand, sidebar: &Sidebar, failed: &BTreeSet<String>) -> Option<RunScope> {
  match command {
    UiCommand::RunAll => {
      let (binaries, planned) = sidebar.binaries_matching(&|_| true);
      (planned > 0).then_some(RunScope {
        grep: None,
        ids: None,
        planned,
        binaries,
      })
    },
    UiCommand::RunGrep(pattern) => {
      let matcher = ferridriver_test::discovery::grep_matcher(pattern);
      let (binaries, planned) = sidebar.binaries_matching(&matcher);
      (planned > 0).then(|| RunScope {
        grep: Some(pattern.clone()),
        ids: None,
        planned,
        binaries,
      })
    },
    UiCommand::RunTest(id) => {
      let (binaries, planned) = sidebar.binaries_matching(&|candidate| candidate == id.as_str());
      (planned > 0).then(|| RunScope {
        grep: Some(format!("^{}$", regex::escape(id))),
        ids: None,
        planned,
        binaries,
      })
    },
    UiCommand::RunFile(file) => {
      let in_file = sidebar.ids_in_file(file);
      (!in_file.is_empty()).then(|| RunScope {
        grep: None,
        planned: in_file.len(),
        binaries: sidebar.binaries_with_file(file),
        ids: Some(in_file),
      })
    },
    UiCommand::RunFailed => {
      let (binaries, _) = sidebar.binaries_matching(&|id| failed.contains(id));
      let known: Vec<String> = sidebar
        .test_ids()
        .into_iter()
        .filter(|id| failed.contains(id))
        .collect();
      let planned = known.len();
      (planned > 0).then_some(RunScope {
        grep: None,
        planned,
        binaries,
        ids: Some(known),
      })
    },
    UiCommand::Stop => None,
  }
}

pub(crate) fn kill_process_group(pid: Option<i32>) {
  let Some(pid) = pid else { return };
  // SAFETY: kill(2) touches no memory; the negative pid signals the
  // whole process group — cargo, the harness binaries, and their
  // browser children — which is exactly what Stop must take down.
  #[allow(unsafe_code)]
  unsafe {
    libc::kill(-pid, libc::SIGKILL);
  }
}

/// Removes the bridge's scratch files (unix socket, id-selection file)
/// when it exits.
struct ScratchCleanup(Vec<PathBuf>);
impl Drop for ScratchCleanup {
  fn drop(&mut self) {
    for path in &self.0 {
      let _ = std::fs::remove_file(path);
    }
  }
}

struct CycleSpawner {
  args: cli::RustTestArgs,
  sock_path: PathBuf,
  /// Scratch file for [`RunScope::ids`] selections, exported as
  /// `FERRITEST_ID_FILE`; rewritten by each cycle that uses it.
  ids_path: PathBuf,
  artifacts_root: PathBuf,
  next_generation: u64,
  done_tx: tokio::sync::mpsc::UnboundedSender<(u64, bool)>,
}

impl CycleSpawner {
  fn spawn(&mut self, kind: CycleKind, scope: Option<RunScope>) -> anyhow::Result<Cycle> {
    // Both cycle kinds run through `cargo test`, never nextest: nextest
    // enumerates binaries via libtest's `--list --format terse`
    // protocol, which ferritest harness binaries do not speak — it sees
    // zero tests and runs nothing.
    let mut cmd = match kind {
      // List cycles pass `-- --list`: libtest binaries in scope print
      // their list instead of running, and harness binaries take the
      // list-only path via either the `--list` flag or FERRITEST_LIST.
      // `--tests` selects every test target while skipping doctests
      // (which cannot hello and would only burn compile time).
      CycleKind::List => {
        let mut cmd = crate::base_test_command(&self.args, cli::TestRunner::Cargo);
        cmd.arg("--tests");
        cmd.arg("--").arg("--list");
        cmd.env(ui_wire::LIST_ENV, "1");
        cmd
      },
      CycleKind::Run => {
        let mut cmd = crate::base_test_command(&self.args, cli::TestRunner::Cargo);
        cmd.env_remove(ui_wire::LIST_ENV);
        cmd
      },
    };
    // Cycle-scoped vars: never inherit stale values from this process's
    // own environment (or the previous cycle).
    cmd.env_remove("FERRITEST_GREP");
    cmd.env_remove("FERRITEST_ID_FILE");
    if let Some(scope) = scope {
      // Bare `cargo test` would run every unit test, integration test,
      // and doctest in the workspace per UI click; the sidebar knows
      // which harness binaries hold the selected tests, so build and
      // run only those targets.
      for binary in &scope.binaries {
        cmd.arg("--test").arg(binary);
      }
      if let Some(ids) = scope.ids {
        std::fs::write(&self.ids_path, ids.join("\n"))?;
        cmd.env("FERRITEST_ID_FILE", &self.ids_path);
      } else if let Some(grep) = scope.grep {
        cmd.env("FERRITEST_GREP", grep);
      }
    }
    cmd
      .env(ui_wire::UI_SOCK_ENV, &self.sock_path)
      .env(ui_wire::UI_ARTIFACTS_ENV, &self.artifacts_root)
      .stdin(Stdio::null());
    // Fresh process group so Stop can kill cargo + harnesses + browsers
    // in one signal without touching this process.
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);

    let mut child = tokio::process::Command::from(cmd).spawn()?;
    let pid = child.id().and_then(|p| i32::try_from(p).ok());
    self.next_generation += 1;
    let generation = self.next_generation;
    let done_tx = self.done_tx.clone();
    tokio::spawn(async move {
      let ok = child.wait().await.is_ok_and(|status| status.success());
      let _ = done_tx.send((generation, ok));
    });
    Ok(Cycle {
      generation,
      pid,
      kind,
      started: std::time::Instant::now(),
      passed: 0,
      failed: 0,
      skipped: 0,
      flaky: 0,
    })
  }
}

/// Forward one harness wire line to browser tabs, maintaining the
/// sidebar, the failed-test set, and the current cycle's counters.
fn handle_wire_message(
  message: &serde_json::Value,
  state: &UiState,
  sidebar: &mut Sidebar,
  failed: &mut BTreeSet<String>,
  cycle: Option<&mut Cycle>,
) {
  match message.get("type").and_then(|t| t.as_str()) {
    Some("testList") => {
      sidebar.record_hello(message);
      state.publish_test_list_message(sidebar.combined());
    },
    // The bridge owns the run boundary: per-binary boundaries would
    // reset tab progress once per participating test binary, and their
    // worker ids collide across processes.
    Some("runStarted" | "runFinished" | "workerStarted" | "workerFinished") => {},
    Some("testFinished") => {
      if let Some(id) = message.get("id").and_then(|i| i.as_str()) {
        let status = message
          .get("outcome")
          .and_then(|o| o.get("status"))
          .and_then(|s| s.as_str())
          .unwrap_or("failed");
        if matches!(status, "passed" | "skipped" | "flaky") {
          failed.remove(id);
        } else {
          failed.insert(id.to_string());
        }
        if let Some(cycle) = cycle {
          match status {
            "passed" => cycle.passed += 1,
            "skipped" => cycle.skipped += 1,
            "flaky" => cycle.flaky += 1,
            _ => cycle.failed += 1,
          }
        }
      }
      state.publish_wire_event(message);
    },
    _ => state.publish_wire_event(message),
  }
}

pub async fn run(config: FerridriverConfig, args: cli::RustTestArgs) -> anyhow::Result<()> {
  if matches!(args.runner, Some(cli::TestRunner::Nextest)) {
    anyhow::bail!(
      "--ui drives ferritest harness binaries through `cargo test`; nextest cannot enumerate \
       them (it sees zero tests via libtest --list). Drop --runner nextest."
    );
  }
  let overrides = ferridriver_test::config::CliOverrides {
    headless: args.headless,
    backend: args.backend.clone(),
    workers: args.workers.map(|n| u32::try_from(n).unwrap_or(u32::MAX)),
    tag: args.tag.clone(),
    retries: args.retries,
    ..Default::default()
  };
  let test_config = ferridriver_test::config::resolve_config_from(config.test, &overrides)
    .map_err(|e| anyhow::anyhow!("config error: {e}"))?;
  // Stop kills cycles with SIGKILL, so their trace spools never drop;
  // reclaim what dead processes left behind before producing our own.
  ferridriver::trace::sweep_stale_spools();
  let cwd = std::env::current_dir()?;
  let artifacts_root = if test_config.output_dir.is_absolute() {
    test_config.output_dir.clone()
  } else {
    cwd.join(&test_config.output_dir)
  };
  std::fs::create_dir_all(&artifacts_root)?;

  let server = UiServer::start(artifacts_root.clone(), args.ui_port)
    .await
    .map_err(|e| anyhow::anyhow!("start UI server: {e}"))?;
  let UiServer {
    addr,
    state,
    mut commands,
  } = server;
  println!("\n  ferridriver UI mode\n\n  http://{addr}\n");

  let sock_path = std::env::temp_dir().join(format!("ferridriver-ui-{}.sock", std::process::id()));
  let ids_path = std::env::temp_dir().join(format!("ferridriver-ui-{}.ids", std::process::id()));
  let _ = std::fs::remove_file(&sock_path);
  let listener = tokio::net::UnixListener::bind(&sock_path)
    .map_err(|e| anyhow::anyhow!("bind UI socket {}: {e}", sock_path.display()))?;
  let _scratch_cleanup = ScratchCleanup(vec![sock_path.clone(), ids_path.clone()]);
  let (wire_tx, mut wire_rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
  tokio::spawn(accept_harness_connections(listener, wire_tx));

  // Watch sources: any .rs edit can affect any test in the crate graph,
  // so changes refresh the sidebar via a list cycle (which recompiles);
  // re-running is left to an explicit UI command, matching Playwright's
  // default (watch off).
  let watcher = ferridriver_test::watch::FileWatcher::new(&cwd, &["**/*.rs".to_string()], &test_config.test_ignore)
    .map_err(|e| anyhow::anyhow!("start file watcher: {e}"))?;

  let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel::<(u64, bool)>();
  let mut spawner = CycleSpawner {
    args,
    sock_path,
    ids_path,
    artifacts_root,
    next_generation: 0,
    done_tx,
  };

  // SIGTERM (kill's default) must also unwind gracefully: the socket
  // file cleanup and the cycle group-kill below only run on a normal
  // loop exit.
  let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

  let workers = test_config.workers;
  let mut sidebar = Sidebar::default();
  let mut failed: BTreeSet<String> = BTreeSet::new();
  let mut queued: VecDeque<UiCommand> = VecDeque::new();
  let mut cycle: Option<Cycle> = None;
  let mut refresh_pending = true;

  loop {
    if cycle.is_none() {
      if let Some(command) = queued.pop_front() {
        if command != UiCommand::Stop
          && let Some(scope) = plan_run(&command, &sidebar, &failed)
        {
          state.set_watch_status("running");
          state.publish_wire_event(&serde_json::json!({
            "type": "runStarted",
            "totalTests": scope.planned,
            "workers": workers,
          }));
          sidebar.hellos_this_cycle.clear();
          match spawner.spawn(CycleKind::Run, Some(scope)) {
            Ok(started) => cycle = Some(started),
            Err(e) => {
              eprintln!("Failed to spawn test cycle: {e}");
              state.set_watch_status("idle");
            },
          }
        }
        continue;
      }
      if refresh_pending {
        refresh_pending = false;
        state.set_watch_status("running");
        sidebar.hellos_this_cycle.clear();
        match spawner.spawn(CycleKind::List, None) {
          Ok(started) => cycle = Some(started),
          Err(e) => {
            eprintln!("Failed to spawn list cycle: {e}");
            state.set_watch_status("idle");
          },
        }
        continue;
      }
    }

    tokio::select! {
      _ = tokio::signal::ctrl_c() => break,
      _ = sigterm.recv() => break,

      message = wire_rx.recv() => {
        let Some(message) = message else { break };
        handle_wire_message(&message, &state, &mut sidebar, &mut failed, cycle.as_mut());
      }

      done = done_rx.recv() => {
        let Some((generation, ok)) = done else { break };
        // Stale completions (a cycle Stop already killed and dropped)
        // are ignored; only the live cycle's exit ends the run.
        if cycle.as_ref().is_none_or(|c| c.generation != generation) {
          continue;
        }
        let finished = cycle.take().unwrap_or_else(|| unreachable!("cycle checked above"));
        match finished.kind {
          CycleKind::List => {
            // A failed list cycle (compile error) produced no hellos —
            // pruning would wipe the sidebar, so keep the last one.
            if ok {
              sidebar.prune_missing();
            }
            state.publish_test_list_message(sidebar.combined());
          },
          CycleKind::Run => {
            let total = finished.passed + finished.failed + finished.skipped + finished.flaky;
            state.publish_wire_event(&serde_json::json!({
              "type": "runFinished",
              "totals": {
                "total": total,
                "passed": finished.passed,
                "failed": finished.failed,
                "skipped": finished.skipped,
                "flaky": finished.flaky,
                "durationMs": u64::try_from(finished.started.elapsed().as_millis()).unwrap_or(u64::MAX),
              },
            }));
          },
        }
        state.set_watch_status("idle");
      }

      change = watcher.recv() => {
        let Some(_) = change else { break };
        let _ = watcher.drain_deduped();
        refresh_pending = true;
      }

      command = commands.recv() => {
        let Some(command) = command else { break };
        if command == UiCommand::Stop {
          if let Some(current) = cycle.take() {
            kill_process_group(current.pid);
            queued.clear();
            state.publish_run_cancelled();
            state.set_watch_status("idle");
          }
        } else if !queued.contains(&command) {
          queued.push_back(command);
        }
      }
    }
  }

  if let Some(current) = cycle.take() {
    kill_process_group(current.pid);
  }
  Ok(())
}

async fn accept_harness_connections(
  listener: tokio::net::UnixListener,
  wire_tx: tokio::sync::mpsc::UnboundedSender<serde_json::Value>,
) {
  loop {
    let Ok((stream, _)) = listener.accept().await else {
      break;
    };
    let tx = wire_tx.clone();
    tokio::spawn(async move {
      use tokio::io::AsyncBufReadExt as _;
      let mut lines = tokio::io::BufReader::new(stream).lines();
      while let Ok(Some(line)) = lines.next_line().await {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
          continue;
        };
        if tx.send(message).is_err() {
          return;
        }
      }
    });
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn sidebar_with(binary: &str, file: &str, ids: &[&str]) -> Sidebar {
    let mut sidebar = Sidebar::default();
    sidebar.record_hello(&serde_json::json!({
      "type": "testList",
      "binary": binary,
      "suites": [{
        "title": "suite",
        "file": file,
        "tests": ids.iter().map(|id| serde_json::json!({
          "id": id, "title": id, "file": file, "status": "idle",
        })).collect::<Vec<_>>(),
      }],
    }));
    sidebar
  }

  #[test]
  fn plan_run_maps_commands_to_scopes() {
    let sidebar = sidebar_with(
      "e2e",
      "tests/e2e.rs",
      &["tests/e2e.rs > a > one", "tests/e2e.rs > a > two"],
    );
    let failed: BTreeSet<String> = ["tests/e2e.rs > a > two".to_string()].into();

    let scope = plan_run(&UiCommand::RunAll, &sidebar, &failed).expect("run all");
    assert_eq!(scope.grep, None);
    assert_eq!(scope.ids, None);
    assert_eq!(scope.planned, 2);
    assert_eq!(scope.binaries, BTreeSet::from(["e2e".to_string()]));

    let scope = plan_run(&UiCommand::RunTest("tests/e2e.rs > a > one".into()), &sidebar, &failed).expect("run test");
    assert_eq!(scope.planned, 1);
    assert_eq!(scope.binaries, BTreeSet::from(["e2e".to_string()]));
    let grep = scope.grep.expect("exact grep");
    assert!(grep.starts_with('^') && grep.ends_with('$'), "anchored: {grep}");
    let re = regex::RegexBuilder::new(&grep)
      .case_insensitive(true)
      .build()
      .expect("valid regex");
    assert!(re.is_match("tests/e2e.rs > a > one"));
    assert!(!re.is_match("tests/e2e.rs > a > two"));
    // Unknown test ids are a no-op, not a phantom 1-test run.
    assert!(plan_run(&UiCommand::RunTest("tests/e2e.rs > a > gone".into()), &sidebar, &failed).is_none());

    // Run-failed and run-file scopes travel as exact id lists (the id
    // file), never as an env-var alternation.
    let scope = plan_run(&UiCommand::RunFailed, &sidebar, &failed).expect("run failed");
    assert_eq!(scope.grep, None);
    assert_eq!(scope.planned, 1);
    assert_eq!(scope.ids.as_deref(), Some(&["tests/e2e.rs > a > two".to_string()][..]));

    let scope = plan_run(&UiCommand::RunFile("tests/e2e.rs".into()), &sidebar, &failed).expect("run file");
    assert_eq!(scope.grep, None);
    assert_eq!(scope.planned, 2);
    assert_eq!(scope.ids.as_ref().map(Vec::len), Some(2));
    assert_eq!(scope.binaries, BTreeSet::from(["e2e".to_string()]));

    assert!(plan_run(&UiCommand::RunGrep("nomatch-xyz".into()), &sidebar, &failed).is_none());
    assert!(plan_run(&UiCommand::RunFailed, &sidebar, &BTreeSet::new()).is_none());
  }

  #[test]
  fn plan_run_grep_uses_filter_semantics_and_scopes_binaries() {
    let mut sidebar = sidebar_with("alpha", "tests/alpha.rs", &["tests/alpha.rs > suite > One"]);
    sidebar.hellos_this_cycle.clear();
    sidebar.record_hello(&serde_json::json!({
      "type": "testList",
      "binary": "beta",
      "suites": [{ "title": "s", "file": "tests/beta.rs", "tests": [
        { "id": "tests/beta.rs > suite > arr[0]", "title": "arr[0]", "file": "tests/beta.rs", "status": "idle" },
      ]}],
    }));

    // Case-insensitive regex; only the matching binary is selected.
    let scope = plan_run(&UiCommand::RunGrep("one".into()), &sidebar, &BTreeSet::new()).expect("grep");
    assert_eq!(scope.planned, 1);
    assert_eq!(scope.binaries, BTreeSet::from(["alpha".to_string()]));

    // Invalid regex falls back to case-insensitive substring.
    let scope = plan_run(&UiCommand::RunGrep("arr[0".into()), &sidebar, &BTreeSet::new()).expect("fallback");
    assert_eq!(scope.planned, 1);
    assert_eq!(scope.binaries, BTreeSet::from(["beta".to_string()]));

    assert!(plan_run(&UiCommand::RunGrep("nope[".into()), &sidebar, &BTreeSet::new()).is_none());
  }

  #[test]
  fn sidebar_merges_binaries_and_prunes_stale_ones() {
    let mut sidebar = sidebar_with("e2e", "tests/e2e.rs", &["tests/e2e.rs > a > one"]);
    sidebar.record_hello(&serde_json::json!({
      "type": "testList",
      "binary": "smoke",
      "suites": [{ "title": "s", "file": "tests/smoke.rs", "tests": [
        { "id": "tests/smoke.rs > s > hi", "title": "hi", "file": "tests/smoke.rs", "status": "idle" },
      ]}],
    }));
    let combined = sidebar.combined();
    assert_eq!(combined["suites"].as_array().map(Vec::len), Some(2));
    assert_eq!(sidebar.test_ids().len(), 2);

    // Next cycle: only `smoke` hellos — `e2e` was deleted/renamed.
    sidebar.hellos_this_cycle.clear();
    sidebar.record_hello(&serde_json::json!({
      "type": "testList",
      "binary": "smoke",
      "suites": [{ "title": "s", "file": "tests/smoke.rs", "tests": [] }],
    }));
    sidebar.prune_missing();
    assert_eq!(sidebar.suites_by_binary.len(), 1);
    assert!(sidebar.suites_by_binary.contains_key("smoke"));
  }

  #[test]
  fn same_stem_binaries_merge_within_a_cycle_and_replace_across_cycles() {
    // Two packages each with a `tests/e2e.rs` produce two executables
    // whose hash-stripped stems collide; both hello in the same cycle.
    let mut sidebar = sidebar_with("e2e", "tests/a.rs", &["tests/a.rs > a > one"]);
    sidebar.record_hello(&serde_json::json!({
      "type": "testList",
      "binary": "e2e",
      "suites": [{ "title": "b", "file": "tests/b.rs", "tests": [
        { "id": "tests/b.rs > b > two", "title": "two", "file": "tests/b.rs", "status": "idle" },
      ]}],
    }));
    assert_eq!(sidebar.test_ids().len(), 2, "second binary appends, not overwrites");

    // A new cycle's first hello for the key replaces the merged list.
    sidebar.hellos_this_cycle.clear();
    sidebar.record_hello(&serde_json::json!({
      "type": "testList",
      "binary": "e2e",
      "suites": [{ "title": "a", "file": "tests/a.rs", "tests": [
        { "id": "tests/a.rs > a > one", "title": "one", "file": "tests/a.rs", "status": "idle" },
      ]}],
    }));
    assert_eq!(sidebar.test_ids().len(), 1, "fresh cycle replaces stale suites");
  }
}
