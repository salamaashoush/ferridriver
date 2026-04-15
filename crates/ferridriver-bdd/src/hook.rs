//! Hook system: lifecycle hooks with tag filtering and ordering.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

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

/// The actual hook function, typed by scope.
pub enum HookHandler {
  /// Global hooks (BeforeAll/AfterAll): no world context.
  Global(Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync>),
  /// Scenario-scoped hooks (BeforeScenario/AfterScenario, BeforeFeature/AfterFeature).
  Scenario(
    Arc<
      dyn for<'a> Fn(&'a mut BrowserWorld) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>
        + Send
        + Sync,
    >,
  ),
  /// Step-scoped hooks (BeforeStep/AfterStep): receive step text.
  Step(
    Arc<
      dyn for<'a> Fn(&'a mut BrowserWorld, &'a str) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>
        + Send
        + Sync,
    >,
  ),
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
  pub fn new() -> Self {
    Self { hooks: Vec::new() }
  }

  /// Register a hook.
  pub fn register(&mut self, hook: Hook) {
    self.hooks.push(hook);
  }

  /// Get all hooks for a given point, filtered by tags, sorted by order.
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
  pub fn get_global(&self, point: HookPoint) -> Vec<&Hook> {
    let mut matched: Vec<&Hook> = self.hooks.iter().filter(|h| h.point == point).collect();
    matched.sort_by_key(|h| h.order);
    matched
  }

  /// Run all global hooks for a given point.
  pub async fn run_global(&self, point: HookPoint) -> Result<(), String> {
    for hook in self.get_global(point) {
      if let HookHandler::Global(handler) = &hook.handler {
        handler().await?;
      }
    }
    Ok(())
  }

  /// Run all scenario hooks for a given point with the given world and tags.
  pub async fn run_scenario(&self, point: HookPoint, world: &mut BrowserWorld, tags: &[String]) -> Result<(), String> {
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
  pub async fn run_suite(&self, point: HookPoint, world: &mut BrowserWorld, tags: &[String]) -> Result<(), String> {
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
  pub async fn run_step(
    &self,
    point: HookPoint,
    world: &mut BrowserWorld,
    step_text: &str,
    tags: &[String],
  ) -> Result<(), String> {
    for hook in self.get(point, tags) {
      match &hook.handler {
        HookHandler::Step(handler) => handler(world, step_text).await?,
        HookHandler::Scenario(handler) => handler(world).await?,
        _ => {},
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

pub fn runtime_hook_point(registration: &ferridriver_test::HookRegistration) -> Option<HookPoint> {
  match (registration.phase, registration.scope) {
    (ferridriver_test::HookPhase::Before, ferridriver_test::HookScope::Suite) => Some(HookPoint::BeforeAll),
    (ferridriver_test::HookPhase::After, ferridriver_test::HookScope::Suite) => Some(HookPoint::AfterAll),
    (ferridriver_test::HookPhase::Before, ferridriver_test::HookScope::Scenario) => Some(HookPoint::BeforeScenario),
    (ferridriver_test::HookPhase::After, ferridriver_test::HookScope::Scenario) => Some(HookPoint::AfterScenario),
    (ferridriver_test::HookPhase::Before, ferridriver_test::HookScope::Step) => Some(HookPoint::BeforeStep),
    (ferridriver_test::HookPhase::After, ferridriver_test::HookScope::Step) => Some(HookPoint::AfterStep),
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
