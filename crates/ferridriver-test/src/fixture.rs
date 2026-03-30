//! Fixture system: dependency-injected, scoped, auto-teardown.
//!
//! Built-in fixtures: `browser` (worker scope), `context` (test scope), `page` (test scope).
//! Custom fixtures can depend on built-ins and each other, forming a DAG.
//!
//! Since core ferridriver types (`Browser`, `Page`, `ContextRef`) do not implement `Clone`,
//! all fixture values are stored as `Arc<T>` and retrieved as `Arc<T>`.

use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use rustc_hash::FxHashMap;
use tokio::sync::{Mutex, RwLock};

use ferridriver::backend::BackendKind;
use ferridriver::options::LaunchOptions;
use ferridriver::Browser;

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
pub type SetupFn = Arc<
  dyn Fn(FixturePool) -> Pin<Box<dyn Future<Output = Result<ArcValue, String>> + Send>> + Send + Sync,
>;

/// Async teardown function: receives the Arc value to clean up.
pub type TeardownFn = Arc<dyn Fn(ArcValue) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Definition of a fixture.
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
/// Each scope level (global, worker, test) has its own pool instance.
/// Child pools inherit from parent pools for cross-scope fixture access.
#[derive(Clone)]
pub struct FixturePool {
  inner: Arc<FixturePoolInner>,
}

struct FixturePoolInner {
  /// Cached fixture values, keyed by name.
  values: RwLock<FxHashMap<String, ArcValue>>,
  /// Fixture definitions (shared reference).
  defs: Arc<FxHashMap<String, FixtureDef>>,
  /// Teardown stack: LIFO order for cleanup.
  teardown_stack: Mutex<Vec<(String, TeardownFn)>>,
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
        values: RwLock::new(FxHashMap::default()),
        defs: Arc::new(defs),
        teardown_stack: Mutex::new(Vec::new()),
        parent: None,
        scope,
      }),
    }
  }

  /// Create a child pool that inherits parent fixtures for cross-scope access.
  pub fn child(&self, scope: FixtureScope) -> Self {
    Self {
      inner: Arc::new(FixturePoolInner {
        values: RwLock::new(FxHashMap::default()),
        defs: Arc::clone(&self.inner.defs),
        teardown_stack: Mutex::new(Vec::new()),
        parent: Some(self.clone()),
        scope,
      }),
    }
  }

  /// Get or lazily create a fixture by name.
  ///
  /// Returns `Arc<T>` since fixture values are shared and not cloneable.
  /// Resolves dependencies recursively (DAG walk).
  ///
  /// # Errors
  ///
  /// Returns an error if the fixture is not defined, setup fails, or type mismatches.
  pub fn get<T: Any + Send + Sync>(&self, name: &str) -> Pin<Box<dyn Future<Output = Result<Arc<T>, String>> + Send>> {
    let pool = self.clone();
    let name = name.to_string();
    Box::pin(async move {
      // Check local cache first.
      {
        let values = pool.inner.values.read().await;
        if let Some(val) = values.get(name.as_str()) {
          return val
            .clone()
            .downcast::<T>()
            .map_err(|_| format!("fixture '{name}' type mismatch"));
        }
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

      let arc_val = tokio::time::timeout(timeout, setup(pool.clone()))
        .await
        .map_err(|_| format!("fixture '{name}' setup timed out after {timeout:?}"))?
        .map_err(|e| format!("fixture '{name}' setup failed: {e}"))?;

      // Cache.
      {
        let mut values = pool.inner.values.write().await;
        values.insert(name.to_string(), Arc::clone(&arc_val));
      }

      // Register teardown.
      if let Some(td) = teardown {
        let mut stack = pool.inner.teardown_stack.lock().await;
        stack.push((name.to_string(), td));
      }

      arc_val
        .downcast::<T>()
        .map_err(|_| format!("fixture '{name}' type mismatch"))
    })
  }

  /// Inject a pre-created fixture value into the pool (skips setup).
  /// Used by the worker to inject persistent page fixtures for performance.
  pub async fn inject<T: Any + Send + Sync>(&self, name: &str, value: Arc<T>) {
    let mut values = self.inner.values.write().await;
    values.insert(name.to_string(), value as ArcValue);
  }

  /// Tear down all fixtures in this pool (reverse order).
  pub async fn teardown_all(&self) {
    let items: Vec<(String, TeardownFn)> = {
      let mut stack = self.inner.teardown_stack.lock().await;
      stack.drain(..).rev().collect()
    };

    for (name, teardown_fn) in items {
      let value = {
        let mut values = self.inner.values.write().await;
        values.remove(&name)
      };
      if let Some(val) = value {
        tracing::debug!("tearing down fixture: {name}");
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
    // Check if already cached.
    {
      let values = pool.inner.values.read().await;
      if values.contains_key(name.as_str()) {
        return Ok(());
      }
    }

    // Check parent scope.
    if let Some(def) = pool.inner.defs.get(name.as_str()) {
      if scope_rank(def.scope) > scope_rank(pool.inner.scope) {
        if let Some(parent) = &pool.inner.parent {
          return ensure_resolved(parent, &name).await;
        }
      }
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

    {
      let mut values = pool.inner.values.write().await;
      values.insert(name.to_string(), arc_val);
    }
    if let Some(td) = teardown {
      let mut stack = pool.inner.teardown_stack.lock().await;
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

// ── DAG Validation ──

/// Validates the fixture dependency graph (no cycles, all deps exist).
/// Returns topological sort order for setup.
///
/// # Errors
///
/// Returns an error if a cycle is detected or a dependency is missing.
pub fn validate_dag(defs: &FxHashMap<String, FixtureDef>) -> Result<Vec<String>, String> {
  let mut in_degree: FxHashMap<&str, usize> = FxHashMap::default();
  let mut adjacency: FxHashMap<&str, Vec<&str>> = FxHashMap::default();

  for (name, def) in defs {
    in_degree.entry(name.as_str()).or_insert(0);
    for dep in &def.dependencies {
      if !defs.contains_key(dep) {
        return Err(format!("fixture '{name}' depends on undefined fixture '{dep}'"));
      }
      adjacency.entry(dep.as_str()).or_default().push(name.as_str());
      *in_degree.entry(name.as_str()).or_insert(0) += 1;
    }
  }

  let mut queue: Vec<&str> = in_degree
    .iter()
    .filter(|(_, deg)| **deg == 0)
    .map(|(name, _)| *name)
    .collect();
  queue.sort();

  let mut order = Vec::new();
  while let Some(node) = queue.pop() {
    order.push(node.to_string());
    if let Some(children) = adjacency.get(node) {
      for child in children {
        if let Some(deg) = in_degree.get_mut(child) {
          *deg -= 1;
          if *deg == 0 {
            queue.push(child);
          }
        }
      }
    }
  }

  if order.len() != defs.len() {
    return Err("fixture dependency cycle detected".to_string());
  }

  Ok(order)
}

// ── Built-in Fixtures ──

/// Create the built-in fixture definitions (browser, context, page).
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
  let viewport = browser_config.viewport.as_ref().map(|v| ferridriver::options::ViewportConfig {
    width: v.width,
    height: v.height,
    ..Default::default()
  });

  // browser (Worker scope) -- stored as Arc<Browser>
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

  // context (Test scope, depends on browser) -- stored as Arc<ContextRef>
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

  // page (Test scope, depends on context) -- stored as Arc<Page>
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
      teardown: None, // Context teardown closes all pages.
      timeout: Duration::from_secs(10),
    },
  );

  defs
}
