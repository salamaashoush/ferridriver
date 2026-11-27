//! Session-shared and lazy-launch worker browser lifecycle handles.

use std::sync::Arc;

use ferridriver::Browser;
use ferridriver::options::LaunchPlan;
use ferridriver::state::{BrowserState, ConnectMode};

use super::TestRunner;

/// The browser a long-lived session keeps between runs, and the plan it
/// was launched from. A project configured for another browser (a
/// different backend, channel, or headedness) must not silently borrow
/// it — [`same_launch`] is what decides.
#[derive(Clone)]
pub(super) struct SharedBrowser {
  pub(super) browser: Arc<Browser>,
  pub(super) plan: LaunchPlan,
  pub(super) connection: ConnectMode,
}

/// Whether two plans would launch the same browser process.
///
/// Only the fields that reach the child matter; context-level options
/// (viewport, locale) ride on the pages, not the process.
pub(super) fn same_launch(a: &LaunchPlan, b: &LaunchPlan) -> bool {
  a.backend == b.backend
    && a.kind == b.kind
    && a.headless == b.headless
    && a.channel == b.channel
    && a.executable_path == b.executable_path
    && a.user_data_dir == b.user_data_dir
    && a.args == b.args
    && a.env == b.env
}

impl TestRunner {
  /// Launch the browser a watch or UI session reuses across its runs,
  /// reporting why when it cannot.
  pub(super) async fn launch_session_browser(&mut self) -> Option<Arc<Browser>> {
    let launch_plan = match build_launch_plan(&self.config.browser) {
      Ok(plan) => plan,
      Err(message) => {
        eprintln!("Error: {message}");
        return None;
      },
    };
    let connection = match self.config.browser.resolve_instance() {
      Ok(connection) => connection.unwrap_or(ConnectMode::Launch),
      Err(error) => {
        tracing::error!("invalid browser connection: {error}");
        return None;
      },
    };
    let browser = match Box::pin(launch_with_plan(launch_plan.clone(), connection.clone())).await {
      Ok(b) => Arc::new(b),
      Err(e) => {
        eprintln!("Failed to launch browser: {e}");
        return None;
      },
    };
    self.shared_browser = Some(SharedBrowser {
      browser: Arc::clone(&browser),
      plan: launch_plan,
      connection,
    });
    Some(browser)
  }
}

pub(super) async fn close_session_browser(browser: &Browser) -> i32 {
  match browser.close().await {
    Ok(()) => 0,
    Err(error) => {
      eprintln!("browser cleanup failed: {error}");
      1
    },
  }
}

/// Build the launch plan for a run.
///
/// # Errors
///
/// Returns a message when the configured instance cannot be resolved
/// (unusable name, failing args command, proxy credentials).
pub(super) fn build_launch_plan(browser_config: &crate::config::BrowserConfig) -> Result<LaunchPlan, String> {
  // BrowserConfig is already normalized (browser↔backend consistent)
  // and validated at load, so the mapping cannot silently downgrade an
  // unrecognised backend here.
  let (backend, kind) = browser_config.resolve_kinds().map_err(|error| error.to_string())?;
  let overrides = browser_config.instance_overrides()?;
  let selection = ferridriver::options::BrowserSelection::resolve(
    overrides.kind.or_else(|| overrides.backend.is_none().then_some(kind)),
    overrides
      .backend
      .or_else(|| overrides.kind.is_none().then_some(backend)),
  )
  .map_err(|error| error.to_string())?;
  let (backend, kind) = (selection.backend, selection.browser);

  let mut args = browser_config.args.clone();
  // `use.proxy` is a per-context option, as Playwright's `proxy` fixture
  // is (`build_context_options`), not a launch flag.
  // Ignore HTTPS errors launch arg.
  if browser_config.use_options.ignore_https_errors {
    args.push("--ignore-certificate-errors".to_string());
  }

  // Force headless under CI even if the config left the default
  // (`false`) in place. Headed Chrome / Firefox on a runner with no
  // DISPLAY hangs the launch handshake past the per-command timeout.
  // Matches Playwright's `process.env.CI` handling in
  // `packages/playwright/src/index.ts` (the `headless` option fixture
  // defaults to `!process.env.PWDEBUG`).
  let headless = browser_config.headless || std::env::var("CI").is_ok();

  args.extend(overrides.args);

  Ok(LaunchPlan {
    backend,
    kind,
    headless: overrides.headless.unwrap_or(headless),
    executable_path: overrides
      .executable_path
      .or_else(|| browser_config.executable_path.clone()),
    user_data_dir: overrides.user_data_dir,
    env: (!overrides.env.is_empty()).then_some(overrides.env),
    ignore_default_args: overrides.ignore_default_args,
    slow_mo: browser_config.slow_mo,
    firefox_user_prefs: (!browser_config.firefox_user_prefs.is_empty()).then(|| {
      browser_config
        .firefox_user_prefs
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
    }),
    args,
    default_viewport: browser_config
      .viewport
      .as_ref()
      .map_or_else(
        || Some(crate::config::ViewportConfig::default()),
        ferridriver_config::browser::ViewportOverride::size,
      )
      .map(|v| ferridriver::options::ViewportConfig {
        width: v.width,
        height: v.height,
        ..Default::default()
      }),
    ..Default::default()
  })
}

/// Launch a browser using the runner's internal `LaunchPlan`. Wraps
/// `BrowserState::with_plan` + `Browser::from_state` so callers don't
/// need to repeat the handshake-await dance.
pub(crate) async fn launch_with_plan(plan: LaunchPlan, connection: ConnectMode) -> ferridriver::error::Result<Browser> {
  let mut state = BrowserState::with_plan(connection, plan);
  Box::pin(state.ensure_browser()).await?;
  Ok(Browser::from_state(state))
}

/// Lazy-launch handle for a worker's browser. The browser is launched
/// on first `get()` call and cached. Workers that never access the
/// browser (e.g. config-only tests) skip the launch entirely — under
/// CI conditions where Chromium first-launch can take >30s, this
/// keeps non-browser tests inside the per-test deadline.
pub struct BrowserHandle {
  plan: LaunchPlan,
  connection: ConnectMode,
  cell: Arc<tokio::sync::OnceCell<Arc<Browser>>>,
  launch: Arc<tokio::sync::Mutex<BrowserLaunchState>>,
  shared: bool,
  /// Worker scope id when borrowing a run-shared Chromium process so each
  /// worker's `browser.contexts()` and `browser.on('context')` stay isolated.
  worker_scope: Option<u64>,
  /// Cached worker-scoped `Browser` view so every `get()` / `try_get()` on
  /// this worker shares the exact same event emitter and disconnect latch.
  scoped_cell: std::sync::OnceLock<Arc<Browser>>,
  /// Raised when a launch fails, so the run can tell a browser that never
  /// started from tests that failed on their own.
  launch_failed: Option<Arc<std::sync::atomic::AtomicBool>>,
  /// `contextReuse`: the browser keeps closed contexts for the next test.
  recycle_contexts: bool,
}

type BrowserLaunch = futures::future::Shared<futures::future::BoxFuture<'static, ferridriver::Result<Arc<Browser>>>>;

#[derive(Default)]
struct BrowserLaunchState {
  closed: bool,
  pending: Option<BrowserLaunch>,
}

impl BrowserHandle {
  #[must_use]
  pub fn new(plan: LaunchPlan) -> Self {
    Self::with_connection(plan, ConnectMode::Launch)
  }

  pub(super) fn with_connection(plan: LaunchPlan, connection: ConnectMode) -> Self {
    Self {
      plan,
      connection,
      cell: Arc::new(tokio::sync::OnceCell::new()),
      launch: Arc::new(tokio::sync::Mutex::default()),
      shared: false,
      worker_scope: None,
      scoped_cell: std::sync::OnceLock::new(),
      launch_failed: None,
      recycle_contexts: false,
    }
  }

  /// Borrow a lazy handle owned by the runner, scoping each worker's
  /// `Browser` view so `browser.contexts()` / `browser.on('context')` remain
  /// isolated per worker and `close()` leaves shutdown to the owner.
  pub(super) fn borrow_shared_scoped(&self, scope: u64) -> Self {
    Self {
      plan: self.plan.clone(),
      connection: self.connection.clone(),
      cell: Arc::clone(&self.cell),
      launch: Arc::clone(&self.launch),
      shared: true,
      worker_scope: Some(scope),
      scoped_cell: std::sync::OnceLock::new(),
      launch_failed: self.launch_failed.clone(),
      recycle_contexts: self.recycle_contexts,
    }
  }

  /// Raise `flag` whenever this handle fails to launch its browser.
  #[must_use]
  pub fn reporting_launch_failure(mut self, flag: Arc<std::sync::atomic::AtomicBool>) -> Self {
    self.launch_failed = Some(flag);
    self
  }

  /// Have the browser this handle launches keep closed contexts for the
  /// next test (`[test] contextReuse`).
  #[must_use]
  pub fn recycling_contexts(mut self, enabled: bool) -> Self {
    self.recycle_contexts = enabled;
    self
  }

  /// Wrap a pre-launched browser (watch-mode shared) — `close()` is a
  /// no-op so the shared browser survives across runs.
  #[must_use]
  pub fn from_shared(browser: Arc<Browser>) -> Self {
    let cell = tokio::sync::OnceCell::new();
    let _ = cell.set(browser);
    Self {
      plan: LaunchPlan::default(),
      connection: ConnectMode::Launch,
      cell: Arc::new(cell),
      launch: Arc::new(tokio::sync::Mutex::default()),
      shared: true,
      worker_scope: None,
      scoped_cell: std::sync::OnceLock::new(),
      launch_failed: None,
      recycle_contexts: false,
    }
  }

  #[tracing::instrument(skip_all, name = "browser_launch")]
  /// The browser if one has already been launched, without launching one.
  ///
  /// For callers that only want to act on a browser that is already driving
  /// (the `--debug` pause), where launching one would be beside the point.
  #[must_use]
  pub fn peek(&self) -> Option<Arc<Browser>> {
    self.cell.get().cloned()
  }

  fn scope_browser(&self, browser: Arc<Browser>) -> Arc<Browser> {
    match self.worker_scope {
      Some(scope) => Arc::clone(
        self
          .scoped_cell
          .get_or_init(|| Arc::new(browser.scoped_for_worker(scope))),
      ),
      None => browser,
    }
  }

  /// The browser, launching it on the first call; concurrent callers
  /// share the one launch.
  ///
  /// # Errors
  ///
  /// Returns [`ferridriver::FerriError::TargetClosed`] once the handle is
  /// closed, or the launch or connection failure.
  pub async fn get(&self) -> ferridriver::error::Result<Arc<Browser>> {
    use futures::FutureExt;
    let pending = {
      let mut state = self.launch.lock().await;
      if state.closed {
        return Err(ferridriver::FerriError::TargetClosed { reason: None });
      }
      if let Some(browser) = self.cell.get() {
        return Ok(self.scope_browser(Arc::clone(browser)));
      }
      state
        .pending
        .get_or_insert_with(|| {
          let plan = self.plan.clone();
          let connection = self.connection.clone();
          let cell = Arc::clone(&self.cell);
          let recycle_contexts = self.recycle_contexts;
          // A cancelled fixture must not abandon a session still being created by the driver.
          let task = tokio::spawn(async move {
            let browser = Arc::new(launch_with_plan(plan, connection).await?);
            browser.set_context_recycling(recycle_contexts).await;
            let _ = cell.set(Arc::clone(&browser));
            Ok(browser)
          });
          async move {
            task
              .await
              .map_err(|error| ferridriver::FerriError::backend(format!("browser launch task failed: {error}")))?
          }
          .boxed()
          .shared()
        })
        .clone()
    };
    let result = pending.clone().await;
    if result.is_err() {
      if let Some(flag) = &self.launch_failed {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
      }
      let mut state = self.launch.lock().await;
      if state.pending.as_ref().is_some_and(|current| current.ptr_eq(&pending)) {
        state.pending = None;
      }
    }
    result.map(|browser| self.scope_browser(browser))
  }

  pub fn try_get(&self) -> Option<Arc<Browser>> {
    self.cell.get().map(|browser| self.scope_browser(Arc::clone(browser)))
  }

  /// Close the browser this handle launched; a shared browser is left to
  /// its owner.
  ///
  /// # Errors
  ///
  /// Returns the launch failure of a launch still in flight, or the
  /// browser's close failure.
  pub async fn close(&self) -> ferridriver::Result<()> {
    if self.shared {
      return Ok(());
    }
    let pending = {
      let mut state = self.launch.lock().await;
      state.closed = true;
      state.pending.clone()
    };
    if let Some(pending) = pending {
      pending.await?.close().await?;
    } else if let Some(browser) = self.cell.get() {
      browser.close().await?;
    }
    Ok(())
  }
}
