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
use ferridriver::backend::BackendKind;
use ferridriver::options::LaunchOptions;

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

/// Type-erased fixture value stored in the pool.
type ArcValue = Arc<dyn Any + Send + Sync>;

/// Async setup function: receives the `FixturePool` (to resolve deps), returns the value.
pub type SetupFn =
  Arc<dyn Fn(FixturePool) -> Pin<Box<dyn Future<Output = Result<ArcValue, String>> + Send>> + Send + Sync>;

/// Async teardown function: receives the Arc value to clean up.
pub type TeardownFn = Arc<dyn Fn(ArcValue) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

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
  /// Teardown stack: LIFO order for cleanup. std::sync::Mutex — only locked briefly.
  teardown_stack: std::sync::Mutex<Vec<(String, TeardownFn)>>,
  /// Parent pool (for cross-scope access).
  parent: Option<FixturePool>,
  /// This pool's scope.
  scope: FixtureScope,
}

impl FixturePool {
  /// Create a new root fixture pool.
  pub fn new(defs: FxHashMap<String, FixtureDef>, scope: FixtureScope) -> Self {
    Self {
      inner: Arc::new(FixturePoolInner {
        values: DashMap::new(),
        defs: Arc::new(defs),
        teardown_stack: std::sync::Mutex::new(Vec::new()),
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
        teardown_stack: std::sync::Mutex::new(Vec::new()),
        parent: Some(self.clone()),
        scope,
      }),
    }
  }

  /// Create a child pool with additional or overridden fixture definitions.
  ///
  /// This is the core building block for per-test fixture graphs: worker/global
  /// fixtures live in the parent pool, while test-scoped fixtures can be
  /// specialized for a single test execution without mutating shared state.
  pub fn child_with_defs(&self, defs: FxHashMap<String, FixtureDef>, scope: FixtureScope) -> Self {
    let mut merged = (*self.inner.defs).clone();
    merged.extend(defs);
    Self {
      inner: Arc::new(FixturePoolInner {
        values: DashMap::new(),
        defs: Arc::new(merged),
        teardown_stack: std::sync::Mutex::new(Vec::new()),
        parent: Some(self.clone()),
        scope,
      }),
    }
  }

  /// Get or lazily create a fixture by name.
  ///
  /// Returns `Arc<T>` since fixture values are shared and not cloneable.
  /// Resolves dependencies recursively (DAG walk).
  pub fn get<T: Any + Send + Sync>(&self, name: &str) -> Pin<Box<dyn Future<Output = Result<Arc<T>, String>> + Send>> {
    let pool = self.clone();
    let name = name.to_string();
    Box::pin(async move {
      // Check local cache first (lock-free read).
      if let Some(val) = pool.inner.values.get(name.as_str()) {
        return val
          .value()
          .clone()
          .downcast::<T>()
          .map_err(|_| format!("fixture '{name}' type mismatch"));
      }

      // Check if this fixture belongs to a parent scope.
      if let Some(def) = pool.inner.defs.get(name.as_str()) {
        if scope_rank(def.scope) > scope_rank(pool.inner.scope) {
          if let Some(parent) = &pool.inner.parent {
            return parent.get::<T>(&name).await;
          }
        }
      } else if let Some(parent) = &pool.inner.parent {
        return parent.get::<T>(&name).await;
      }

      // Resolve dependencies first.
      if let Some(def) = pool.inner.defs.get(name.as_str()) {
        for dep in &def.dependencies {
          ensure_resolved(&pool, dep).await?;
        }
      }

      // Set up the fixture.
      let def = pool
        .inner
        .defs
        .get(name.as_str())
        .ok_or_else(|| format!("fixture '{name}' not defined"))?;

      let setup = Arc::clone(&def.setup);
      let teardown = def.teardown.as_ref().map(Arc::clone);
      let timeout = def.timeout;

      tracing::debug!(target: "ferridriver::fixture", fixture = name, "setting up fixture");
      let arc_val = tokio::time::timeout(timeout, setup(pool.clone()))
        .await
        .map_err(|_| format!("fixture '{name}' setup timed out after {timeout:?}"))?
        .map_err(|e| format!("fixture '{name}' setup failed: {e}"))?;

      // Cache (lock-free insert).
      pool.inner.values.insert(name.to_string(), Arc::clone(&arc_val));

      // Register teardown.
      if let Some(td) = teardown {
        let mut stack = pool.inner.teardown_stack.lock().unwrap();
        stack.push((name.to_string(), td));
      }

      arc_val
        .downcast::<T>()
        .map_err(|_| format!("fixture '{name}' type mismatch"))
    })
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
  pub async fn resolve(&self, name: &str) -> Result<(), String> {
    ensure_resolved(self, name).await
  }

  /// Tear down all fixtures in this pool (reverse order).
  pub async fn teardown_all(&self) {
    let items: Vec<(String, TeardownFn)> = {
      let mut stack = self.inner.teardown_stack.lock().unwrap();
      stack.drain(..).rev().collect()
    };

    for (name, teardown_fn) in items {
      let value = self.inner.values.remove(&name).map(|(_, v)| v);
      if let Some(val) = value {
        tracing::debug!(target: "ferridriver::fixture", "tearing down fixture: {name}");
        teardown_fn(val).await;
      }
    }
  }
}

/// Ensure a fixture is resolved (trigger creation without needing a concrete type).
fn ensure_resolved(pool: &FixturePool, name: &str) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> {
  let pool = pool.clone();
  let name = name.to_string();
  Box::pin(async move {
    // Check if already cached (lock-free read).
    if pool.inner.values.contains_key(name.as_str()) {
      return Ok(());
    }

    // Check parent scope.
    if let Some(def) = pool.inner.defs.get(name.as_str()) {
      if scope_rank(def.scope) > scope_rank(pool.inner.scope) {
        if let Some(parent) = &pool.inner.parent {
          return ensure_resolved(parent, &name).await;
        }
      }
    } else if let Some(parent) = &pool.inner.parent {
      return ensure_resolved(parent, &name).await;
    }

    // Resolve dependencies.
    if let Some(def) = pool.inner.defs.get(name.as_str()) {
      for dep in &def.dependencies {
        ensure_resolved(&pool, dep).await?;
      }
    }

    // Set up.
    let def = pool
      .inner
      .defs
      .get(name.as_str())
      .ok_or_else(|| format!("fixture '{name}' not defined"))?;
    let setup = Arc::clone(&def.setup);
    let teardown = def.teardown.as_ref().map(Arc::clone);
    let timeout = def.timeout;

    let arc_val = tokio::time::timeout(timeout, setup(pool.clone()))
      .await
      .map_err(|_| format!("fixture '{name}' setup timed out after {timeout:?}"))?
      .map_err(|e| format!("fixture '{name}' setup failed: {e}"))?;

    pool.inner.values.insert(name.to_string(), arc_val);
    if let Some(td) = teardown {
      let mut stack = pool.inner.teardown_stack.lock().unwrap();
      stack.push((name.to_string(), td));
    }
    Ok(())
  })
}

fn scope_rank(scope: FixtureScope) -> u8 {
  match scope {
    FixtureScope::Test => 0,
    FixtureScope::Worker => 1,
    FixtureScope::Global => 2,
  }
}

/// Validate that fixture definitions form a DAG (no cycles).
pub fn validate_dag(defs: &FxHashMap<String, FixtureDef>) -> Result<(), String> {
  use std::collections::HashSet;

  fn visit(
    name: &str,
    defs: &FxHashMap<String, FixtureDef>,
    visiting: &mut HashSet<String>,
    visited: &mut HashSet<String>,
  ) -> Result<(), String> {
    if visited.contains(name) {
      return Ok(());
    }
    if !visiting.insert(name.to_string()) {
      return Err(format!("circular fixture dependency involving '{name}'"));
    }
    if let Some(def) = defs.get(name) {
      for dep in &def.dependencies {
        visit(dep, defs, visiting, visited)?;
      }
    }
    visiting.remove(name);
    visited.insert(name.to_string());
    Ok(())
  }

  let mut visiting = HashSet::new();
  let mut visited = HashSet::new();
  for name in defs.keys() {
    visit(name, defs, &mut visiting, &mut visited)?;
  }
  Ok(())
}

/// Built-in fixture definitions for the ferridriver test runner.
pub fn builtin_fixtures(browser_config: &BrowserConfig) -> FxHashMap<String, FixtureDef> {
  let mut defs = FxHashMap::default();

  let backend = match browser_config.backend.as_str() {
    "cdp-raw" => BackendKind::CdpRaw,
    #[cfg(target_os = "macos")]
    "webkit" => BackendKind::WebKit,
    _ => BackendKind::CdpPipe,
  };
  let headless = browser_config.headless;
  let executable_path = browser_config.executable_path.clone();
  let args = browser_config.args.clone();
  let viewport = browser_config
    .viewport
    .as_ref()
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
          let browser = Browser::launch(LaunchOptions {
            backend,
            headless,
            executable_path: exec,
            args: extra_args,
            viewport: vp,
            ..Default::default()
          })
          .await
          .map_err(|e| format!("failed to launch browser: {e}"))?;
          Ok(Arc::new(browser) as ArcValue)
        })
      }),
      teardown: Some(Arc::new(|val| {
        Box::pin(async move {
          if let Ok(browser) = val.downcast::<Browser>() {
            let _ = browser.close().await;
          }
        })
      })),
      timeout: Duration::from_secs(30),
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
          let context = browser.new_context();
          Ok(Arc::new(context) as ArcValue)
        })
      }),
      teardown: Some(Arc::new(|val| {
        Box::pin(async move {
          if let Ok(ctx) = val.downcast::<ferridriver::ContextRef>() {
            let _ = ctx.close().await;
          }
        })
      })),
      timeout: Duration::from_secs(10),
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
          let page = context
            .new_page()
            .await
            .map_err(|e| format!("failed to create page: {e}"))?;
          Ok(Arc::new(page) as ArcValue)
        })
      }),
      teardown: None,
      timeout: Duration::from_secs(10),
    },
  );

  defs
}
