//! Hook system: lifecycle hooks with tag filtering and ordering.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use ferridriver::error::Result;

use crate::filter::TagExpression;
use crate::step::StepLocation;
use crate::world::BrowserWorld;

// ── Hook points ──

/// When a hook fires in the BDD lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookPoint {
  BeforeAll,
  AfterAll,
  BeforeFeature,
  AfterFeature,
  BeforeScenario,
  AfterScenario,
  BeforeStep,
  AfterStep,
}

// ── Hook handler variants ──

/// What every hook returns.
pub type HookFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
/// A hook with no world context (`BeforeAll`/`AfterAll`).
pub type GlobalHookFn = Arc<dyn Fn() -> HookFuture<'static> + Send + Sync>;
/// A hook that runs against the scenario's world.
pub type ScenarioHookFn = Arc<dyn for<'a> Fn(&'a mut BrowserWorld) -> HookFuture<'a> + Send + Sync>;
/// A hook that runs against the world and the step's text.
pub type StepHookFn = Arc<dyn for<'a> Fn(&'a mut BrowserWorld, &'a str) -> HookFuture<'a> + Send + Sync>;

/// The actual hook function, typed by scope.
pub enum HookHandler {
  /// Global hooks (BeforeAll/AfterAll): no world context.
  Global(GlobalHookFn),
  /// Scenario-scoped hooks (BeforeScenario/AfterScenario, BeforeFeature/AfterFeature).
  Scenario(ScenarioHookFn),
  /// Step-scoped hooks (BeforeStep/AfterStep): receive step text.
  Step(StepHookFn),
}

// ── Hook definition ──

/// A registered hook with metadata.
pub struct Hook {
  /// When this hook fires.
  pub point: HookPoint,
  /// Optional tag filter expression (e.g., "@smoke and not @wip").
  /// If `None`, the hook runs for all scenarios.
  pub tag_filter: Option<TagExpression>,
  /// Execution order (lower runs first). Default: 0.
  pub order: i32,
  /// The handler function.
  pub handler: HookHandler,
  /// Source location for diagnostics.
  pub location: StepLocation,
}

// ── Hook registry ──

/// Collects and dispatches hooks.
pub struct HookRegistry {
  hooks: Vec<Hook>,
}

impl HookRegistry {
  #[must_use]
  pub fn new() -> Self {
    Self { hooks: Vec::new() }
  }

  /// Register a hook.
  pub fn register(&mut self, hook: Hook) {
    self.hooks.push(hook);
  }

  /// Get all hooks for a given point, filtered by tags, sorted by order.
  #[must_use]
  pub fn get(&self, point: HookPoint, tags: &[String]) -> Vec<&Hook> {
    let mut matched: Vec<&Hook> = self
      .hooks
      .iter()
      .filter(|h| h.point == point)
      .filter(|h| match &h.tag_filter {
        None => true,
        Some(expr) => expr.matches(tags),
      })
      .collect();

    matched.sort_by_key(|h| h.order);
    matched
  }

  /// Get global hooks (no tag filtering needed).
  #[must_use]
  pub fn get_global(&self, point: HookPoint) -> Vec<&Hook> {
    let mut matched: Vec<&Hook> = self.hooks.iter().filter(|h| h.point == point).collect();
    matched.sort_by_key(|h| h.order);
    matched
  }

  /// Run all global hooks for a given point.
  ///
  /// # Errors
  ///
  /// The first hook error; later hooks do not run.
  pub async fn run_global(&self, point: HookPoint) -> Result<()> {
    for hook in self.get_global(point) {
      if let HookHandler::Global(handler) = &hook.handler {
        handler().await?;
      }
    }
    Ok(())
  }

  /// Run all scenario hooks for a given point with the given world and tags.
  ///
  /// # Errors
  ///
  /// The first hook error; later hooks do not run.
  pub async fn run_scenario(&self, point: HookPoint, world: &mut BrowserWorld, tags: &[String]) -> Result<()> {
    for hook in self.get(point, tags) {
      if let HookHandler::Scenario(handler) = &hook.handler {
        handler(world).await?;
      }
    }
    Ok(())
  }

  /// Run suite-level hooks for a given point.
  ///
  /// `BeforeAll` / `AfterAll` may come from either a world-aware TS hook or
  /// a world-less Rust hook, so this dispatch supports both variants.
  ///
  /// # Errors
  ///
  /// The first hook error; later hooks do not run.
  pub async fn run_suite(&self, point: HookPoint, world: &mut BrowserWorld, tags: &[String]) -> Result<()> {
    for hook in self.get(point, tags) {
      match &hook.handler {
        HookHandler::Scenario(handler) => handler(world).await?,
        HookHandler::Global(handler) => handler().await?,
        HookHandler::Step(_) => {},
      }
    }
    Ok(())
  }

  /// Run all step hooks for a given point.
  ///
  /// # Errors
  ///
  /// The first hook error; later hooks do not run.
  pub async fn run_step(
    &self,
    point: HookPoint,
    world: &mut BrowserWorld,
    step_text: &str,
    tags: &[String],
  ) -> Result<()> {
    for hook in self.get(point, tags) {
      match &hook.handler {
        HookHandler::Step(handler) => handler(world, step_text).await?,
        HookHandler::Scenario(handler) => handler(world).await?,
        HookHandler::Global(_) => {},
      }
    }
    Ok(())
  }
}

impl Default for HookRegistry {
  fn default() -> Self {
    Self::new()
  }
}

// ── Inventory registration ──

/// What the `#[before]` / `#[after]` proc macros submit via `inventory::submit!`.
pub struct HookRegistration {
  pub point: HookPoint,
  pub tag_filter: Option<String>,
  pub order: i32,
  pub handler_factory: fn() -> HookHandler,
  pub file: &'static str,
  pub line: u32,
}

inventory::collect!(HookRegistration);

/// Convenience macro for submitting hook registrations from proc macro expansion.
#[macro_export]
macro_rules! submit_hook {
  ($name:ident, $point:expr, $tag_filter:expr, $order:expr, $handler:ident,) => {
    ferridriver_bdd::inventory::submit! {
      ferridriver_bdd::hook::HookRegistration {
        point: $point,
        tag_filter: $tag_filter,
        order: $order,
        handler_factory: $handler,
        file: file!(),
        line: line!(),
      }
    }
  };
}
