//! TypeScript test-file front-end for the ferridriver test runner.
//!
//! Mirrors `ferridriver-bdd`'s role for `.test.ts`/`.spec.ts` files:
//! discover files from `[test].testMatch`, rolldown-bundle them once to
//! `QuickJS` bytecode, evaluate the bundle in a collection session to
//! snapshot every `test`/`describe` registration, translate the
//! snapshot into a core [`TestPlan`], and execute each body through a
//! per-worker `QuickJS` session — all inside the single
//! `ferridriver-test` `TestRunner` pipeline (workers, fixtures,
//! retries, reporters, tracing).
//!
//! This is the only crate that depends on both `ferridriver-script`
//! and `ferridriver-test`; it contains no rquickjs code — every VM
//! interaction goes through the typed surface `ferridriver-script`
//! exports (`collect_tests`, `run_test`, `run_standalone_hook`,
//! [`ferridriver_script::TestHostBridge`]).

mod translate;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use dashmap::DashMap;
use ferridriver_script::{CollectedTests, CompiledBundle, bundle_and_compile_named, collect_tests, eval_bundle};
use ferridriver_test::config::{CliOverrides, TestConfig};
use ferridriver_test::model::TestPlan;
use tokio::sync::OnceCell;

pub use ferridriver_test::host::InfoBridge;
pub use translate::translate_tests;

/// Bundle module label — appears in stack frames before source-map
/// remap and namespaces the bytecode disk cache.
const BUNDLE_NAME: &str = "ferridriver-tests.js";

/// The `[scripting]` sandbox caps the test VM runs with. Set once by
/// the `ferridriver test` entry point from resolved config; unset ⇒
/// locked down ([`ferridriver_script::ScriptCaps::default`]).
static TEST_SCRIPT_CAPS: OnceLock<ferridriver_script::ScriptCaps> = OnceLock::new();

/// Install the test VM sandbox caps. Call before the run; idempotent
/// (first set wins).
pub fn set_test_script_caps(caps: ferridriver_script::ScriptCaps) {
  let _ = TEST_SCRIPT_CAPS.set(caps);
}

/// Declared sidecar specs the test VM exposes as
/// `sidecars.connect(name)`. Same threading as
/// [`set_test_script_caps`].
static TEST_SIDECARS: OnceLock<Vec<ferridriver_script::sidecar::SidecarSpec>> = OnceLock::new();

pub fn set_test_sidecars(sidecars: Vec<ferridriver_script::sidecar::SidecarSpec>) {
  let _ = TEST_SIDECARS.set(sidecars);
}

/// Extensions the test VMs load, compiled once for the run. Same
/// threading as [`set_test_script_caps`]; unset ⇒ none, which is what a
/// harness binary with no config gets.
static TEST_EXTENSIONS: OnceLock<Vec<ferridriver_script::ExtensionBinding>> = OnceLock::new();

pub fn set_test_extensions(extensions: Vec<ferridriver_script::ExtensionBinding>) {
  let _ = TEST_EXTENSIONS.set(extensions);
}

/// The bindings every test VM installs — the collection session and each
/// worker alike, because a fixture or matcher an extension contributes
/// has to exist where tests are COLLECTED as well as where they run.
fn test_extensions() -> Vec<ferridriver_script::ExtensionBinding> {
  TEST_EXTENSIONS.get().cloned().unwrap_or_default()
}

/// The engine config a test VM runs with. One function, because the
/// collection session and a worker diverging is how a registration
/// index shifts between the two.
fn test_engine_config(
  console: Option<Arc<dyn ferridriver_script::ConsoleSink>>,
) -> ferridriver_script::ScriptEngineConfig {
  ferridriver_script::ScriptEngineConfig {
    sidecars: TEST_SIDECARS.get().cloned().unwrap_or_default(),
    console_sink: console,
    ..Default::default()
  }
}

/// Discover test entry files for the config's `testMatch` globs,
/// resolved against `cwd` (and `testDir` when set). `.feature` globs
/// belong to the BDD path and are skipped here; `testIgnore` prunes.
#[must_use]
pub fn discover_test_files(config: &TestConfig, cwd: &Path) -> Vec<PathBuf> {
  let base = match &config.test_dir {
    Some(dir) => {
      let p = Path::new(dir);
      if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) }
    },
    None => cwd.to_path_buf(),
  };
  let ignore: Vec<glob::Pattern> = config
    .test_ignore
    .iter()
    .filter_map(|p| glob::Pattern::new(p).ok())
    .collect();
  let mut files = Vec::new();
  for pat in &config.test_match {
    if pat.ends_with(".feature") || pat.contains(".feature") {
      continue;
    }
    let full = if Path::new(pat).is_absolute() {
      pat.clone()
    } else {
      base.join(pat).to_string_lossy().into_owned()
    };
    if let Ok(entries) = glob::glob(&full) {
      for entry in entries.flatten() {
        if !ferridriver_script::is_source_file(&entry) {
          continue;
        }
        let rel = entry.strip_prefix(cwd).unwrap_or(&entry);
        if ignore.iter().any(|ig| ig.matches_path(rel) || ig.matches_path(&entry)) {
          continue;
        }
        files.push(entry);
      }
    }
  }
  files.sort();
  files.dedup();
  files
}

/// A loaded per-worker test session: one `QuickJS` VM with the bundled
/// test module evaluated (registrations live in the VM's registry).
pub struct JsTestSession {
  session: ferridriver_script::Session,
  console: Arc<TestConsole>,
}

/// Routes a spec's `console.*` to the test that printed it.
///
/// One per worker session: the console global is installed per VM, while
/// the output belongs to whichever test that VM is running. The test
/// invocation binds its buffer around the body and unbinds after, so a
/// line printed between tests (module top level, a stray timer) is
/// dropped rather than charged to the wrong test.
#[derive(Default)]
pub struct TestConsole {
  /// The running test, which owns both the buffer the line lands in and
  /// the bus it is published on.
  target: std::sync::Mutex<Option<Arc<ferridriver_test::model::TestInfo>>>,
}

impl std::fmt::Debug for TestConsole {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("TestConsole").finish_non_exhaustive()
  }
}

impl TestConsole {
  fn bind(&self, test_info: Arc<ferridriver_test::model::TestInfo>) {
    if let Ok(mut target) = self.target.lock() {
      *target = Some(test_info);
    }
  }

  fn unbind(&self) {
    if let Ok(mut target) = self.target.lock() {
      *target = None;
    }
  }
}

impl ferridriver_script::ConsoleSink for TestConsole {
  fn emit(&self, entry: &ferridriver_script::ConsoleEntry) {
    use ferridriver_script::ConsoleLevel;
    let Ok(target) = self.target.lock() else { return };
    let Some(test_info) = target.as_ref() else { return };
    // Node's split, which Playwright's reporters assume: warnings and
    // errors are stderr, everything else stdout.
    let stderr = matches!(entry.level, ConsoleLevel::Warn | ConsoleLevel::Error);
    test_info.emit_output(stderr, &entry.message);
  }
}

/// What a module registered, compared between the collection pass and
/// every worker. The fixture-set count matters as much as the test
/// count: a body resolves its fixtures by set INDEX, so a chain built
/// under a condition would silently point at another chain's fixtures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registrations {
  tests: usize,
  fixture_sets: usize,
}

impl Registrations {
  fn of(collected: &ferridriver_script::CollectedTests) -> Self {
    Self {
      tests: collected.tests.len(),
      fixture_sets: collected.fixture_sets.len(),
    }
  }
}

impl JsTestSession {
  /// Create the worker session and evaluate the precompiled bundle.
  /// The registration count is checked against the collection pass —
  /// a mismatch means test files register nondeterministically
  /// (registration inside conditionals on ambient state), which would
  /// desync body indices from the plan.
  ///
  /// # Errors
  ///
  /// Fails when the session cannot be created, the bundle fails to
  /// evaluate, or the registration count diverges from the plan's.
  pub async fn load(bundle: Arc<CompiledBundle>, cwd: &Path, expected: Registrations) -> anyhow::Result<Self> {
    let sandbox = Arc::new(
      ferridriver_script::PathSandbox::new(cwd)
        .map_err(|e| anyhow::anyhow!("sandbox {}: {}", cwd.display(), e.message))?,
    );
    let run_ctx = ferridriver_script::RunContext {
      vars: Arc::new(ferridriver_script::InMemoryVars::new()),
      sandbox,
      artifacts: None,
      page: None,
      browser_context: None,
      request: None,
      browser: None,
      extensions: test_extensions(),
      host: ferridriver_script::ExtensionHost::Test,
      caps: TEST_SCRIPT_CAPS.get().cloned().unwrap_or_default(),
      session: None,
    };
    let console = Arc::new(TestConsole::default());
    let engine_config = test_engine_config(Some(Arc::clone(&console) as Arc<dyn ferridriver_script::ConsoleSink>));
    let session = ferridriver_script::Session::create(engine_config, &run_ctx)
      .await
      .map_err(|e| anyhow::anyhow!("session create: {}", e.message))?;
    let vm = session.vm_handle();
    eval_bundle(&vm, &bundle)
      .await
      .map_err(|e| anyhow::anyhow!("test bundle failed to load: {}", bundle.format_error(&e)))?;
    let collected = collect_tests(&vm)
      .await
      .map_err(|e| anyhow::anyhow!("collect tests: {}", e.message))?;
    let registered = Registrations::of(&collected);
    if registered != expected {
      anyhow::bail!(
        "test registration is nondeterministic: collection saw {} tests and {} fixture sets, this worker \
         registered {} and {} — register tests and build `test.extend` / `mergeTests` chains unconditionally \
         at module top level",
        expected.tests,
        expected.fixture_sets,
        registered.tests,
        registered.fixture_sets
      );
    }
    Ok(Self { session, console })
  }

  #[must_use]
  pub fn session(&self) -> &ferridriver_script::Session {
    &self.session
  }

  /// Send this session's `console.*` to `output` for the duration of one
  /// test; [`ConsoleBinding`] unbinds when it drops.
  pub(crate) fn capture_console(&self, test_info: Arc<ferridriver_test::model::TestInfo>) -> ConsoleBinding<'_> {
    self.console.bind(test_info);
    ConsoleBinding { console: &self.console }
  }

  #[must_use]
  pub fn vm_handle(&self) -> ferridriver_script::VmHandle {
    self.session.vm_handle()
  }
}

/// Per-plan worker-session pool: one `QuickJS` VM per worker index, owned
/// by the plan that created it. A pool per run (instead of a process
/// global) keeps concurrent `TestRunner` runs — parallel projects, the
/// runner's own tests — from evicting each other's live VMs, and makes
/// watch-mode invalidation trivial (new plan ⇒ new pool).
/// Holds a session's console on one test's output. Unbinds on drop, so a
/// body that panics or times out cannot leave the next test's lines going
/// to a finished test.
pub struct ConsoleBinding<'a> {
  console: &'a TestConsole,
}

impl Drop for ConsoleBinding<'_> {
  fn drop(&mut self) {
    self.console.unbind();
  }
}

pub struct SessionPool {
  bundle: Arc<CompiledBundle>,
  cwd: Arc<PathBuf>,
  expected: Registrations,
  slots: DashMap<u32, Arc<OnceCell<Arc<JsTestSession>>>>,
}

impl SessionPool {
  fn new(bundle: Arc<CompiledBundle>, cwd: PathBuf, expected: Registrations) -> Self {
    Self {
      bundle,
      cwd: Arc::new(cwd),
      expected,
      slots: DashMap::new(),
    }
  }

  pub(crate) async fn get(&self, worker_index: u32) -> Result<Arc<JsTestSession>, String> {
    let cell = Arc::clone(
      &self
        .slots
        .entry(worker_index)
        .or_insert_with(|| Arc::new(OnceCell::new())),
    );
    let bundle = Arc::clone(&self.bundle);
    let cwd = Arc::clone(&self.cwd);
    let expected = self.expected;
    cell
      .get_or_try_init(|| async move {
        JsTestSession::load(bundle, &cwd, expected)
          .await
          .map(Arc::new)
          .map_err(|e| e.to_string())
      })
      .await
      .cloned()
  }

  /// Drop a worker's session so the next test rebuilds it.
  ///
  /// A force-halt (timeout interrupt) or an allocation fault stops the
  /// interpreter wherever it was, so everything the VM holds — module
  /// state, a half-applied fixture chain, a half-written global — is
  /// suspect. The registrations still look intact, which is exactly why
  /// this cannot be left to a health check: the next `get` re-evaluates
  /// the bundle and `JsTestSession::load` re-verifies the registration
  /// counts against the collection snapshot before any test runs in it.
  ///
  /// Re-loading is also what defines what an extension's module-level
  /// state means across a poison: it is rebuilt from the bundle, not
  /// carried over.
  pub fn poison(&self, worker_index: u32) {
    if self.slots.remove(&worker_index).is_some() {
      tracing::warn!(
        target: "ferridriver::testjs",
        worker = worker_index,
        "worker VM was force-halted; rebuilding it before the next test",
      );
    }
  }

  /// Resume every suspended worker-scoped fixture and drop the cached
  /// sessions. Call once after `TestRunner::run` returns.
  pub async fn teardown(&self) {
    let entries: Vec<(u32, Arc<OnceCell<Arc<JsTestSession>>>)> = {
      let mut out = Vec::new();
      for r in &self.slots {
        out.push((*r.key(), Arc::clone(r.value())));
      }
      self.slots.clear();
      out
    };
    for (worker, cell) in entries {
      if let Some(session) = cell.get()
        && let Err(e) = ferridriver_script::teardown_worker_fixtures(&session.vm_handle()).await
      {
        tracing::warn!(target: "ferridriver::testjs", worker, error = %e.message, "worker fixture teardown failed");
      }
    }
  }
}

/// The compiled bundle + collection snapshot a plan is built from.
pub struct TsTestSource {
  pub bundle: Arc<CompiledBundle>,
  pub collected: CollectedTests,
  pub files: Vec<PathBuf>,
}

/// Discover, bundle and collect — everything up to translation. Returns
/// `None` when no test files match (the caller decides whether that is
/// an error).
///
/// # Errors
///
/// Fails when bundling, the collection session, or bundle evaluation
/// fails.
pub async fn load_ts_tests(config: &TestConfig, cwd: &Path) -> anyhow::Result<Option<TsTestSource>> {
  let files = discover_test_files(config, cwd);
  if files.is_empty() {
    return Ok(None);
  }
  let bundle = Arc::new(
    bundle_and_compile_named(&files, cwd, BUNDLE_NAME)
      .await
      .map_err(|e| anyhow::anyhow!("bundle test files: {}", e.message))?,
  );

  // Collection session: evaluate once, snapshot registrations. The
  // session is discarded — workers build their own.
  let sandbox = Arc::new(
    ferridriver_script::PathSandbox::new(cwd)
      .map_err(|e| anyhow::anyhow!("sandbox {}: {}", cwd.display(), e.message))?,
  );
  let run_ctx = ferridriver_script::RunContext {
    vars: Arc::new(ferridriver_script::InMemoryVars::new()),
    sandbox,
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: test_extensions(),
    host: ferridriver_script::ExtensionHost::Test,
    caps: TEST_SCRIPT_CAPS.get().cloned().unwrap_or_default(),
    session: None,
  };
  // Built from the SAME engine config a worker uses: sidecars and the
  // console sink change what an extension's top level can do, and a
  // collection VM that differs from a worker's is how the two come to
  // disagree about what was registered.
  let session = ferridriver_script::Session::create(test_engine_config(None), &run_ctx)
    .await
    .map_err(|e| anyhow::anyhow!("collection session: {}", e.message))?;
  let vm = session.vm_handle();
  eval_bundle(&vm, &bundle)
    .await
    .map_err(|e| anyhow::anyhow!("test bundle failed to load: {}", bundle.format_error(&e)))?;
  let collected = collect_tests(&vm)
    .await
    .map_err(|e| anyhow::anyhow!("collect tests: {}", e.message))?;
  Ok(Some(TsTestSource {
    bundle,
    collected,
    files,
  }))
}

/// Discover + bundle + collect + translate in one call — the plan the
/// `ferridriver test` CLI feeds to `TestRunner::run`, plus the session
/// pool to tear down after the run.
///
/// # Errors
///
/// Propagates [`load_ts_tests`] and [`translate_tests`] failures.
pub async fn build_ts_plan(config: &TestConfig, cwd: &Path) -> anyhow::Result<Option<(TestPlan, Arc<SessionPool>)>> {
  let Some(source) = load_ts_tests(config, cwd).await? else {
    return Ok(None);
  };
  let pool = Arc::new(SessionPool::new(
    Arc::clone(&source.bundle),
    cwd.to_path_buf(),
    Registrations::of(&source.collected),
  ));
  let plan = translate_tests(&source, config, cwd, &pool)?;
  Ok(Some((plan, pool)))
}

/// Default discovery globs when neither config nor CLI narrowed them —
/// the Playwright convention.
const DEFAULT_TEST_MATCH: &[&str] = &["**/*.spec.ts", "**/*.test.ts"];

fn empty_plan() -> TestPlan {
  TestPlan {
    suites: Vec::new(),
    total_tests: 0,
    shard: None,
  }
}

/// The `ferridriver test` entry point: resolve discovery globs, build
/// the plan, and run it through the core `TestRunner` — plain runs and
/// `--watch`/`--ui` (each cycle re-bundles and swaps in a fresh
/// session pool, tearing down the previous cycle's).
pub async fn run_ts_tests_with(mut config: TestConfig, overrides: CliOverrides) -> i32 {
  let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

  // Extensions load ONCE for the run, through the gate every host uses,
  // and the bindings serve the collection session and every worker. The
  // CLI has populated `overrides.extensions` since extensions existed;
  // until now nothing on this host read it.
  if !overrides.extensions.is_empty() && TEST_EXTENSIONS.get().is_none() {
    let caps = TEST_SCRIPT_CAPS.get().cloned().unwrap_or_default();
    let sidecar_names: Vec<String> = TEST_SIDECARS
      .get()
      .map(|s| s.iter().map(|s| s.name.clone()).collect())
      .unwrap_or_default();
    let env = ferridriver_script::RequirementEnv::from_caps(&caps, &sidecar_names);
    set_test_extensions(
      ferridriver_script::load_bindings(
        &overrides.extensions,
        &env,
        &caps.extension_policy,
        ferridriver_script::ExtensionHost::Test,
      )
      .await,
    );
  }

  // Positional files/globs on the CLI replace the config's testMatch.
  if !overrides.test_files.is_empty() {
    config.test_match = overrides.test_files.clone();
  }
  if config.test_match.is_empty() {
    config.test_match = DEFAULT_TEST_MATCH.iter().map(|s| (*s).to_string()).collect();
  }

  if overrides.ui || overrides.watch {
    let ui_mode = overrides.ui;
    let ui_port = overrides.ui_port;
    let factory_config = config.clone();
    let factory_cwd = cwd.clone();
    // Each watch cycle owns a fresh pool; the previous cycle's suspended
    // worker fixtures are resumed before its sessions drop.
    let live_pool: Arc<Mutex<Option<Arc<SessionPool>>>> = Arc::new(Mutex::new(None));
    let factory: ferridriver_test::runner::WatchPlanFactory = Box::new(move |_changed| {
      let config = factory_config.clone();
      let cwd = factory_cwd.clone();
      let live_pool = Arc::clone(&live_pool);
      Box::pin(async move {
        let previous = live_pool
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .take();
        if let Some(pool) = previous {
          pool.teardown().await;
        }
        match build_ts_plan(&config, &cwd).await {
          Ok(Some((plan, pool))) => {
            *live_pool.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pool);
            ferridriver_test::runner::PlanBuild::ok(plan)
          },
          Ok(None) => {
            eprintln!("no test files found (testMatch: {:?})", config.test_match);
            ferridriver_test::runner::PlanBuild::ok(empty_plan())
          },
          Err(e) => ferridriver_test::runner::PlanBuild::failed(empty_plan(), e.to_string()),
        }
      })
    });
    let mut runner = ferridriver_test::runner::TestRunner::new(config, overrides);
    return if ui_mode {
      Box::pin(runner.run_test_server(factory, cwd, None, ui_port)).await
    } else {
      runner.run_watch(factory, cwd).await
    };
  }

  let (plan, pool) = match build_ts_plan(&config, &cwd).await {
    Ok(Some(built)) => built,
    Ok(None) => {
      eprintln!("no test files found (testMatch: {:?})", config.test_match);
      return i32::from(!overrides.pass_with_no_tests);
    },
    Err(e) => {
      eprintln!("{e}");
      return 1;
    },
  };
  let code = ferridriver_test::runner::TestRunner::new(config, overrides)
    .run(plan)
    .await;
  pool.teardown().await;
  code
}

#[cfg(test)]
mod tests {
  use super::*;
  use ferridriver_script::{ConsoleEntry, ConsoleLevel, ConsoleSink};

  fn entry(level: ConsoleLevel, message: &str) -> ConsoleEntry {
    ConsoleEntry {
      level,
      message: message.to_string(),
      ts_ms: 0,
    }
  }

  #[test]
  fn console_output_goes_to_the_test_that_printed_it() {
    let console = TestConsole::default();
    let test_info = Arc::new(ferridriver_test::model::TestInfo::new_anonymous());
    let output = Arc::clone(&test_info.output);

    // Nothing is bound yet: a line printed between tests belongs to no
    // test rather than to the next one.
    console.emit(&entry(ConsoleLevel::Log, "module top level"));

    console.bind(Arc::clone(&test_info));
    console.emit(&entry(ConsoleLevel::Log, "hello"));
    console.emit(&entry(ConsoleLevel::Warn, "careful"));
    console.emit(&entry(ConsoleLevel::Error, "boom"));
    console.unbind();
    console.emit(&entry(ConsoleLevel::Log, "after the test"));

    let held = output.lock().expect("output");
    assert_eq!(held.stdout, "hello\n");
    assert_eq!(held.stderr, "careful\nboom\n", "warnings and errors are stderr");
  }
}
