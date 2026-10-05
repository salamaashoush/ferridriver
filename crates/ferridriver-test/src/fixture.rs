//! Fixture system: dependency-injected, scoped, auto-teardown.
//!
//! Built-in fixtures: `browser` (worker scope), `context` (test scope), `page` (test scope).
//! Custom fixtures can depend on built-ins and each other, forming a DAG.
//!
//! Uses lock-free DashMap for fixture values — zero contention on concurrent reads.

use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use rustc_hash::FxHashMap;

use ferridriver::Browser;
use ferridriver::options::LaunchPlan;
use ferridriver::state::{BrowserState, ConnectMode};

use crate::config::BrowserConfig;

// ── Types ──

/// Fixture lifecycle scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FixtureScope {
  /// Created fresh for each test, torn down after.
  Test,
  /// Shared across all tests in a single worker.
  Worker,
  /// Shared across all workers (global setup/teardown).
  Global,
}

impl FixtureScope {
  /// The name this scope is written as in a `{ scope: … }` option bag
  /// and printed as in a diagnostic.
  #[must_use]
  pub fn label(self) -> &'static str {
    match self {
      Self::Test => "test",
      Self::Worker => "worker",
      Self::Global => "global",
    }
  }

  /// Parse the name a host wrote, or `None` when it is not a scope.
  #[must_use]
  pub fn from_label(name: &str) -> Option<Self> {
    match name {
      "test" => Some(Self::Test),
      "worker" => Some(Self::Worker),
      "global" => Some(Self::Global),
      _ => None,
    }
  }

  /// Widening rank: a fixture may depend only on one that lives at
  /// least as long as it does (`Test` < `Worker` < `Global`).
  #[must_use]
  pub fn rank(self) -> u8 {
    match self {
      Self::Test => 0,
      Self::Worker => 1,
      Self::Global => 2,
    }
  }
}

/// Type-erased fixture value stored in the pool.
type ArcValue = Arc<dyn Any + Send + Sync>;

/// Async setup function: receives the `FixturePool` (to resolve deps), returns the value.
pub type SetupFn =
  Arc<dyn Fn(FixturePool) -> Pin<Box<dyn Future<Output = ferridriver::error::Result<ArcValue>> + Send>> + Send + Sync>;

/// Async teardown function: receives the Arc value to clean up.
type TeardownFuture = Pin<Box<dyn Future<Output = ferridriver::Result<()>> + Send>>;
pub type TeardownFn = Arc<dyn Fn(ArcValue) -> TeardownFuture + Send + Sync>;

struct Teardown {
  name: String,
  value: ArcValue,
  callback: TeardownFn,
  pending: tokio::sync::Mutex<Option<TeardownFuture>>,
}

/// A fixture value paired with an optional teardown, returned from
/// `#[fixture]` bodies that need cleanup when their scope ends:
///
/// ```ignore
/// #[fixture(scope = "worker")]
/// async fn db(_ctx: TestContext) -> ferridriver_test::Result<Fixture<DbHandle>> {
///     let db = DbHandle::connect().await?;
///     Ok(Fixture::new(db).on_teardown(|db| async move { db.drop_schema().await; }))
/// }
/// ```
///
/// The teardown receives the shared `Arc<T>` when the fixture's scope
/// tears down (reverse setup order, like Playwright fixture cleanup).
pub struct Fixture<T> {
  value: T,
  teardown: Option<Box<dyn FnOnce(Arc<T>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>>,
}

impl<T: Any + Send + Sync> Fixture<T> {
  pub fn new(value: T) -> Self {
    Self { value, teardown: None }
  }

  /// Attach an async teardown, run when the fixture's scope ends.
  #[must_use]
  pub fn on_teardown<F, Fut>(mut self, teardown: F) -> Self
  where
    F: FnOnce(Arc<T>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
  {
    self.teardown = Some(Box::new(move |value| Box::pin(teardown(value))));
    self
  }

  /// Split into the value and a pool-registrable teardown. Consumed by
  /// the `#[fixture]` macro expansion.
  #[must_use]
  pub fn into_parts(self) -> (T, Option<TeardownFn>) {
    let teardown = self.teardown.map(|f| {
      let cell = std::sync::Mutex::new(Some(f));
      Arc::new(move |value: ArcValue| -> TeardownFuture {
        let f = cell.lock().ok().and_then(|mut guard| guard.take());
        match (f, value.downcast::<T>()) {
          (Some(f), Ok(typed)) => Box::pin(async move {
            f(typed).await;
            Ok(())
          }),
          _ => Box::pin(async {
            Err(ferridriver::FerriError::backend(
              "fixture teardown was consumed or received the wrong value type",
            ))
          }),
        }
      }) as TeardownFn
    });
    (self.value, teardown)
  }
}

impl<T: Any + Send + Sync> From<T> for Fixture<T> {
  fn from(value: T) -> Self {
    Self::new(value)
  }
}

/// Definition of a fixture.
#[derive(Clone)]
pub struct FixtureDef {
  pub name: String,
  pub scope: FixtureScope,
  /// Names of fixtures this one depends on.
  pub dependencies: Vec<String>,
  pub setup: SetupFn,
  pub teardown: Option<TeardownFn>,
  /// Timeout for setup.
  pub timeout: Duration,
  /// Playwright `auto: true` semantic — the fixture must resolve for
  /// every test (and every hook at the matching scope) regardless of
  /// whether the body asks for it. The worker enumerates all auto
  /// fixtures at scope-entry time and resolves them before the test
  /// body runs.
  pub auto: bool,
}

// ── Fixture Pool ──

/// Runtime cache of instantiated fixtures with scoped lifecycle management.
///
/// Uses lock-free DashMap for fixture values — concurrent reads never block.
/// Each scope level (global, worker, test) has its own pool instance.
/// Child pools inherit from parent pools for cross-scope fixture access.
#[derive(Clone)]
pub struct FixturePool {
  inner: Arc<FixturePoolInner>,
}

struct FixturePoolInner {
  /// Cached fixture values — lock-free concurrent map.
  values: DashMap<String, ArcValue>,
  /// Fixture definitions (shared reference).
  defs: Arc<FxHashMap<String, FixtureDef>>,
  /// The same definitions as the workspace's one fixture graph sees
  /// them, so ordering, cycles, auto fixtures and the scope rule are
  /// decided by `fixture_graph` here exactly as they are for a JS
  /// `test.extend` chain or a BDD scenario.
  slots: Arc<Vec<crate::fixture_graph::FixtureSlot>>,
  /// Teardown stack: LIFO order for cleanup. std::sync::Mutex — only locked briefly.
  teardown_stack: std::sync::Mutex<Vec<Arc<Teardown>>>,
  teardown_lock: tokio::sync::Mutex<()>,
  /// Parent pool (for cross-scope access).
  parent: Option<FixturePool>,
  /// This pool's scope.
  scope: FixtureScope,
}

impl FixturePool {
  /// Create a new root fixture pool.
  pub fn new(defs: FxHashMap<String, FixtureDef>, scope: FixtureScope) -> Self {
    let slots = Arc::new(slots_of(&defs));
    Self {
      inner: Arc::new(FixturePoolInner {
        values: DashMap::new(),
        defs: Arc::new(defs),
        slots,
        teardown_stack: std::sync::Mutex::new(Vec::new()),
        teardown_lock: tokio::sync::Mutex::new(()),
        parent: None,
        scope,
      }),
    }
  }

  /// Create a child pool that inherits parent fixtures for cross-scope access.
  pub fn child(&self, scope: FixtureScope) -> Self {
    Self {
      inner: Arc::new(FixturePoolInner {
        values: DashMap::new(),
        defs: Arc::clone(&self.inner.defs),
        slots: Arc::clone(&self.inner.slots),
        teardown_stack: std::sync::Mutex::new(Vec::new()),
        teardown_lock: tokio::sync::Mutex::new(()),
        parent: Some(self.clone()),
        scope,
      }),
    }
  }

  /// Register a teardown to run when this pool's scope ends (reverse
  /// registration order). The `#[fixture]` macro calls this for
  /// [`Fixture`]-returning bodies; the teardown retains this exact value
  /// even if its cache entry is replaced before cleanup starts.
  pub fn register_teardown(&self, name: &str, value: ArcValue, teardown: TeardownFn) {
    let mut stack = self
      .inner
      .teardown_stack
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    stack.push(Arc::new(Teardown {
      name: name.to_string(),
      value,
      callback: teardown,
      pending: tokio::sync::Mutex::new(None),
    }));
  }

  /// Create a child pool with additional or overridden fixture definitions.
  ///
  /// This is the core building block for per-test fixture graphs: worker/global
  /// fixtures live in the parent pool, while test-scoped fixtures can be
  /// specialized for a single test execution without mutating shared state.
  pub fn child_with_defs(&self, defs: FxHashMap<String, FixtureDef>, scope: FixtureScope) -> Self {
    let mut merged = (*self.inner.defs).clone();
    merged.extend(defs);
    let slots = Arc::new(slots_of(&merged));
    Self {
      inner: Arc::new(FixturePoolInner {
        values: DashMap::new(),
        defs: Arc::new(merged),
        slots,
        teardown_stack: std::sync::Mutex::new(Vec::new()),
        teardown_lock: tokio::sync::Mutex::new(()),
        parent: Some(self.clone()),
        scope,
      }),
    }
  }

  #[must_use]
  pub fn scope_root(&self, scope: FixtureScope) -> Option<Self> {
    let mut current = Some(self);
    let mut owner = None;
    while let Some(pool) = current {
      if pool.inner.scope == scope {
        owner = Some(pool.clone());
      }
      current = pool.inner.parent.as_ref();
    }
    owner
  }

  /// Get or lazily create a fixture by name.
  ///
  /// Returns `Arc<T>` since fixture values are shared and not cloneable.
  /// Dependencies are set up first, in the order `fixture_graph` gives.
  pub fn get<T: Any + Send + Sync>(
    &self,
    name: &str,
  ) -> Pin<Box<dyn Future<Output = ferridriver::error::Result<Arc<T>>> + Send>> {
    let pool = self.clone();
    let name = name.to_string();
    Box::pin(async move {
      use ferridriver::FerriError;
      ensure_resolved(&pool, &name).await?;
      pool
        .cached_value(&name)
        .ok_or_else(|| FerriError::backend(format!("fixture '{name}' not defined")))?
        .downcast::<T>()
        .map_err(|_| FerriError::backend(format!("fixture '{name}' type mismatch")))
    })
  }

  /// The value of `name` if this pool or any parent has resolved it.
  fn cached_value(&self, name: &str) -> Option<ArcValue> {
    if let Some(val) = self.inner.values.get(name) {
      return Some(val.value().clone());
    }
    self.inner.parent.as_ref().and_then(|p| p.cached_value(name))
  }

  /// Whether `name` exists without a registration of its own here — an
  /// injected value, or a definition a parent pool owns. It is what
  /// separates a legitimate override from a self-reference with nothing
  /// underneath it.
  fn is_provided(&self, name: &str) -> bool {
    self.inner.values.contains_key(name)
      || self
        .inner
        .parent
        .as_ref()
        .is_some_and(|p| p.inner.defs.contains_key(name) || p.is_provided(name))
  }

  /// Synchronously get an already-resolved fixture from the cache.
  /// Returns None if the fixture hasn't been resolved yet.
  /// Lock-free DashMap read — no async needed.
  /// Used by NAPI lazy fixture getters to avoid redundant async resolution.
  pub fn try_get_cached<T: Any + Send + Sync>(&self, name: &str) -> Option<Arc<T>> {
    if let Some(val) = self.inner.values.get(name) {
      val.value().clone().downcast::<T>().ok()
    } else if let Some(parent) = &self.inner.parent {
      parent.try_get_cached::<T>(name)
    } else {
      None
    }
  }

  /// Inject a pre-created fixture value into the pool (skips setup).
  /// Lock-free DashMap insert — no async needed.
  pub fn inject<T: Any + Send + Sync>(&self, name: &str, value: Arc<T>) {
    self.inner.values.insert(name.to_string(), value as ArcValue);
  }

  /// Resolve a fixture by name without knowing its concrete type.
  pub async fn resolve(&self, name: &str) -> ferridriver::error::Result<()> {
    ensure_resolved(self, name).await
  }

  pub(crate) async fn resolve_worker_dependencies(&self, names: &[String]) -> ferridriver::Result<()> {
    let order = crate::fixture_graph::dependency_order(&self.inner.slots, names, &|name| self.is_provided(name))
      .map_err(|error| ferridriver::FerriError::invalid_argument("fixture", error))?;
    for pos in order {
      let slot = &self.inner.slots[pos];
      if slot.scope == FixtureScope::Worker {
        self.resolve(&slot.name).await?;
      }
    }
    Ok(())
  }

  /// Names of every fixture marked `auto: true` whose scope matches the
  /// argument or any narrower scope (Test fixtures get included for
  /// Test pools; Worker auto fixtures get included for Worker pools).
  /// Walks the parent chain so worker-scope auto fixtures are visible
  /// from a test-scope child pool.
  #[must_use]
  pub fn auto_fixture_names_for(&self, scope: FixtureScope) -> Vec<String> {
    let mut names: Vec<String> = crate::fixture_graph::auto_slots(&self.inner.slots, scope)
      .into_iter()
      .map(|pos| self.inner.slots[pos].name.clone())
      .collect();
    if let Some(parent) = &self.inner.parent {
      for n in parent.auto_fixture_names_for(scope) {
        if !names.contains(&n) {
          names.push(n);
        }
      }
    }
    names
  }

  /// Tear down all fixtures in this pool (reverse order).
  pub async fn teardown_all(&self) -> ferridriver::Result<()> {
    let _cleanup = self.inner.teardown_lock.lock().await;
    loop {
      let item = self
        .inner
        .teardown_stack
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .last()
        .cloned();
      let Some(item) = item else { return Ok(()) };
      let mut pending = item.pending.lock().await;
      tracing::debug!(target: "ferridriver::fixture", fixture = item.name, "tearing down fixture");
      let future = pending.get_or_insert_with(|| teardown_future(Arc::clone(&item.callback), Arc::clone(&item.value)));
      let result = future.await;
      pending.take();
      result.map_err(|error| {
        ferridriver::FerriError::backend(format!("fixture '{}' teardown failed: {error}", item.name))
      })?;
      self
        .inner
        .values
        .remove_if(&item.name, |_, current| Arc::ptr_eq(current, &item.value));
      self
        .inner
        .teardown_stack
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|entry| !Arc::ptr_eq(entry, &item));
    }
  }
}

fn teardown_future(callback: TeardownFn, value: ArcValue) -> TeardownFuture {
  use futures::FutureExt;
  Box::pin(async move {
    std::panic::AssertUnwindSafe(async move { callback(value).await })
      .catch_unwind()
      .await
      .unwrap_or_else(|payload| {
        let message = payload
          .downcast_ref::<String>()
          .map(String::as_str)
          .or_else(|| payload.downcast_ref::<&str>().copied())
          .unwrap_or("non-string panic");
        Err(ferridriver::FerriError::backend(format!(
          "fixture teardown panicked: {message}"
        )))
      })
  })
}

/// Ensure a fixture is resolved (trigger creation without needing a
/// concrete type), together with everything it depends on.
///
/// Ordering, cycles, the self-reference rule and the scope rule all come
/// from [`crate::fixture_graph`] — the same code a JS `test.extend`
/// chain and a BDD scenario resolve through.
fn ensure_resolved(
  pool: &FixturePool,
  name: &str,
) -> Pin<Box<dyn Future<Output = ferridriver::error::Result<()>> + Send>> {
  let pool = pool.clone();
  let name = name.to_string();
  Box::pin(async move {
    use ferridriver::FerriError;
    if pool.cached_value(&name).is_some() {
      return Ok(());
    }
    // Not ours: a parent scope owns it (or nobody does).
    let Some(def) = pool.inner.defs.get(name.as_str()) else {
      return match &pool.inner.parent {
        Some(parent) => ensure_resolved(parent, &name).await,
        None => Err(FerriError::backend(format!("fixture '{name}' not defined"))),
      };
    };
    if def.scope.rank() > pool.inner.scope.rank()
      && let Some(parent) = &pool.inner.parent
    {
      return ensure_resolved(parent, &name).await;
    }

    let order =
      crate::fixture_graph::dependency_order(&pool.inner.slots, std::slice::from_ref(&name), &|n| pool.is_provided(n))
        .map_err(|e| FerriError::invalid_argument("fixture", e))?;
    for pos in order {
      let slot = &pool.inner.slots[pos];
      if pool.cached_value(&slot.name).is_some() {
        continue;
      }
      if slot.scope.rank() > pool.inner.scope.rank()
        && let Some(parent) = &pool.inner.parent
      {
        ensure_resolved(parent, &slot.name).await?;
        continue;
      }
      set_up(&pool, &slot.name).await?;
    }
    Ok(())
  })
}

/// Run one fixture's setup in this pool, cache the value and park its
/// teardown. Every path into a fixture goes through here.
async fn set_up(pool: &FixturePool, name: &str) -> ferridriver::error::Result<()> {
  use ferridriver::FerriError;
  let def = pool
    .inner
    .defs
    .get(name)
    .ok_or_else(|| FerriError::backend(format!("fixture '{name}' not defined")))?;
  let setup = Arc::clone(&def.setup);
  let teardown = def.teardown.as_ref().map(Arc::clone);
  let timeout = def.timeout;

  tracing::debug!(target: "ferridriver::fixture", fixture = name, "setting up fixture");
  let result = if timeout.is_zero() {
    setup(pool.clone()).await
  } else {
    ferridriver::pause::run_within(timeout, setup(pool.clone()))
      .await
      .map_err(|_| FerriError::timeout(format!("fixture '{name}' setup"), timeout.as_millis() as u64))?
  };
  let arc_val = result.map_err(|e| FerriError::backend(format!("fixture '{name}' setup failed: {e}")))?;

  if let Some(td) = teardown {
    pool.register_teardown(name, Arc::clone(&arc_val), td);
  }
  pool.inner.values.insert(name.to_string(), arc_val);
  Ok(())
}

/// Lower fixture definitions into the graph's slots. Name order, since a
/// Rust registration table is keyed by name and holds exactly one entry
/// per name — the shadowing chains a `test.extend` builds have no Rust
/// equivalent, and a stable order keeps setup sequences reproducible.
fn slots_of(defs: &FxHashMap<String, FixtureDef>) -> Vec<crate::fixture_graph::FixtureSlot> {
  let mut names: Vec<&String> = defs.keys().collect();
  names.sort_unstable();
  names
    .into_iter()
    .enumerate()
    .map(|(reg, name)| {
      let def = &defs[name];
      crate::fixture_graph::FixtureSlot {
        reg,
        name: name.clone(),
        deps: def.dependencies.clone(),
        auto: def.auto,
        scope: def.scope,
        // A Rust `#[fixture]` has no option form: its value comes from
        // the function, never from a `use` block.
        option: false,
      }
    })
    .collect()
}

/// Validate that fixture definitions form a DAG (no cycles) and that no
/// fixture depends on one that does not outlive it.
///
/// # Errors
///
/// The reason from [`crate::fixture_graph`], which is the same text a JS
/// chain or a BDD scenario would report for the same shape.
pub fn validate_dag(defs: &FxHashMap<String, FixtureDef>) -> ferridriver::error::Result<()> {
  let slots = slots_of(defs);
  let names: Vec<String> = slots.iter().map(|s| s.name.clone()).collect();
  crate::fixture_graph::dependency_order(&slots, &names, &|_| false)
    .map(|_| ())
    .map_err(|e| ferridriver::FerriError::invalid_argument("fixture", e))
}

/// Built-in fixture definitions for the ferridriver test runner.
/// # Errors
/// Rejects an invalid browser selection before creating fixture definitions.
pub fn builtin_fixtures(browser_config: &BrowserConfig) -> ferridriver::error::Result<FxHashMap<String, FixtureDef>> {
  let mut defs = FxHashMap::default();

  let (backend, kind) = browser_config.resolve_kinds()?;
  let headless = browser_config.headless;
  let executable_path = browser_config.executable_path.clone();
  let args = browser_config.args.clone();
  let viewport = browser_config
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
    });

  // browser (Worker scope)
  defs.insert(
    "browser".into(),
    FixtureDef {
      name: "browser".into(),
      scope: FixtureScope::Worker,
      dependencies: vec![],
      setup: Arc::new(move |_pool| {
        let exec = executable_path.clone();
        let extra_args = args.clone();
        let vp = viewport.clone();
        Box::pin(async move {
          let plan = LaunchPlan {
            backend,
            kind,
            headless,
            executable_path: exec,
            args: extra_args,
            default_viewport: vp,
            ..Default::default()
          };
          let mut state = BrowserState::with_plan(ConnectMode::Launch, plan);
          Box::pin(state.ensure_browser()).await?;
          let browser = Browser::from_state(state);
          Ok(Arc::new(browser) as ArcValue)
        })
      }),
      teardown: Some(Arc::new(|val| {
        Box::pin(async move {
          let browser = val
            .downcast::<Browser>()
            .map_err(|_| ferridriver::FerriError::backend("browser fixture has the wrong value type"))?;
          browser.close().await
        })
      })),
      timeout: Duration::from_secs(30),
      auto: false,
    },
  );

  // context (Test scope, depends on browser)
  defs.insert(
    "context".into(),
    FixtureDef {
      name: "context".into(),
      scope: FixtureScope::Test,
      dependencies: vec!["browser".into()],
      setup: Arc::new(|pool| {
        Box::pin(async move {
          let browser: Arc<Browser> = pool.get("browser").await?;
          let context = browser.new_context().await?;
          Ok(Arc::new(context) as ArcValue)
        })
      }),
      teardown: Some(Arc::new(|val| {
        Box::pin(async move {
          let context = val
            .downcast::<ferridriver::ContextRef>()
            .map_err(|_| ferridriver::FerriError::backend("context fixture has the wrong value type"))?;
          context.close().await
        })
      })),
      timeout: Duration::from_secs(10),
      auto: false,
    },
  );

  // page (Test scope, depends on context)
  defs.insert(
    "page".into(),
    FixtureDef {
      name: "page".into(),
      scope: FixtureScope::Test,
      dependencies: vec!["context".into()],
      setup: Arc::new(|pool| {
        Box::pin(async move {
          let context: Arc<ferridriver::ContextRef> = pool.get("context").await?;
          let page = context.new_page().await?;
          Ok(Arc::new(page) as ArcValue)
        })
      }),
      teardown: None,
      timeout: Duration::from_secs(10),
      auto: false,
    },
  );

  Ok(defs)
}

#[cfg(test)]
mod setup_tests {
  use super::*;
  use std::sync::atomic::{AtomicUsize, Ordering};

  #[tokio::test]
  async fn worker_prerequisites_are_lazy_shared_and_outside_the_body_budget() {
    let starts = Arc::new(AtomicUsize::new(0));
    let count = starts.clone();
    let worker = FixtureDef {
      name: "browser".into(),
      scope: FixtureScope::Worker,
      dependencies: vec![],
      setup: Arc::new(move |_| {
        let count = count.clone();
        Box::pin(async move {
          tokio::time::sleep(Duration::from_millis(20)).await;
          count.fetch_add(1, Ordering::SeqCst);
          Ok(Arc::new(42_u32) as ArcValue)
        })
      }),
      teardown: None,
      timeout: Duration::ZERO,
      auto: false,
    };
    let page = FixtureDef {
      name: "page".into(),
      scope: FixtureScope::Test,
      dependencies: vec!["browser".into()],
      setup: Arc::new(|_| Box::pin(async { Err(ferridriver::FerriError::backend("test setup must stay lazy")) })),
      teardown: None,
      timeout: Duration::from_secs(1),
      auto: false,
    };
    let parent = FixturePool::new(FxHashMap::from_iter([("browser".into(), worker)]), FixtureScope::Worker);
    let pool = parent.child_with_defs(FxHashMap::from_iter([("page".into(), page)]), FixtureScope::Test);
    pool.resolve_worker_dependencies(&[]).await.unwrap();
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    pool.resolve_worker_dependencies(&["page".into()]).await.unwrap();
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert!(pool.try_get_cached::<u32>("page").is_none());
    pool.resolve_worker_dependencies(&["page".into()]).await.unwrap();
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    let browser = ferridriver::pause::run_within(Duration::from_millis(1), pool.get::<u32>("browser"))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(*browser, 42);
  }
}
