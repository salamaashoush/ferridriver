//! BDD tools -- run Gherkin feature files and individual steps on live MCP sessions.

use crate::server::{McpServer, sess};
use rmcp::{
  ErrorData,
  handler::server::wrapper::Parameters,
  model::{CallToolResult, Content},
  tool, tool_router,
};
use serde::Deserialize;

use ferridriver_bdd::scenario::{ScenarioStatus, StepStatus};
use ferridriver_bdd::world::BrowserWorld;

// ── Parameter types ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RunScenarioParams {
  #[schemars(
    description = "Gherkin feature text (inline) or absolute file path to a .feature file. \
    Inline example: 'Feature: Login\\n  Scenario: Valid login\\n    Given I navigate to \"...\"\\n    When I fill \"#email\" with \"user@example.com\"'. \
    File example: '/path/to/login.feature'."
  )]
  pub feature: String,
  #[schemars(
    description = "Run only scenarios whose name contains this substring (case-insensitive). Omit to run all scenarios in the feature."
  )]
  pub scenario: Option<String>,
  #[schemars(
    description = "Tag filter expression using boolean logic. Examples: '@smoke', '@smoke and not @wip', '@login or @signup'. Omit to run all scenarios regardless of tags."
  )]
  pub tags: Option<String>,
  #[schemars(
    description = "Browser session to run against. The scenario executes on the session's current page with its existing cookies and state. Defaults to 'default'."
  )]
  pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RunStepParams {
  #[schemars(
    description = "A single BDD step to execute, written in natural language matching a registered step definition. \
    Examples: 'I navigate to \"https://example.com\"', 'I click \"Submit\"', 'I fill \"#email\" with \"test@example.com\"', \
    '\"h1\" should contain text \"Welcome\"'. Use list_steps to discover all available step patterns."
  )]
  pub step: String,
  #[schemars(
    description = "Browser session to execute the step on. Uses the session's current page and state. Defaults to 'default'."
  )]
  pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListStepsParams {
  #[schemars(
    description = "Filter steps whose expression contains this text (case-insensitive). Example: 'click' shows all click-related steps. Omit to list all steps."
  )]
  pub filter: Option<String>,
  #[schemars(
    description = "Filter by step kind: 'given', 'when', 'then', or 'step' (keyword-agnostic). Omit to show all kinds."
  )]
  pub kind: Option<String>,
}

// ── Tool implementations ────────────────────────────────────────────────────

#[tool_router(router = bdd_router, vis = "pub")]
impl McpServer {
  #[tool(
    name = "run_scenario",
    description = "Execute a BDD/Gherkin feature on the current browser session. \
    Parses inline Gherkin text or reads a .feature file, then runs each scenario step-by-step on the live page. \
    Returns per-step pass/fail results and a final accessibility snapshot. \
    Use this to run structured test sequences written in Gherkin (Given/When/Then). \
    For running a single action, use run_step instead. For raw browser actions, use click/fill/navigate directly."
  )]
  async fn run_scenario(&self, Parameters(p): Parameters<RunScenarioParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_ref());
    let _guard = self.session_guard(s).await;

    // Parse feature -- inline text or file path.
    let feature_set = if p.feature.trim_start().starts_with("Feature:") || p.feature.trim_start().starts_with('@') {
      ferridriver_bdd::feature::FeatureSet::parse_text(&p.feature)
        .map_err(|e| McpServer::err(format!("Failed to parse Gherkin: {e}")))?
    } else {
      let path = std::path::PathBuf::from(&p.feature);
      if !path.exists() {
        return Err(McpServer::err(format!("Feature file not found: {}", p.feature)));
      }
      ferridriver_bdd::feature::FeatureSet::parse(vec![path])
        .map_err(|e| McpServer::err(format!("Failed to parse feature file: {e}")))?
    };

    if feature_set.features.is_empty() {
      return Err(McpServer::err("No features found"));
    }

    // Expand and filter scenarios.
    let mut scenarios = Vec::new();
    for feature in &feature_set.features {
      scenarios.extend(ferridriver_bdd::scenario::expand_feature(feature));
    }

    if let Some(ref name_filter) = p.scenario {
      let lower = name_filter.to_lowercase();
      scenarios.retain(|s| s.name.to_lowercase().contains(&lower));
    }

    if let Some(ref tag_expr) = p.tags {
      if let Ok(expr) = ferridriver_bdd::filter::TagExpression::parse(tag_expr) {
        scenarios.retain(|s| expr.matches(&s.tags));
      }
    }

    if scenarios.is_empty() {
      return Ok(CallToolResult::success(vec![Content::text(
        "No scenarios matched the given filters.",
      )]));
    }

    // Single page + context acquisition for all scenarios.
    let (page, context_ref) = Box::pin(self.page_and_context(s)).await?;

    // Reuse one BrowserWorld across scenarios -- reset between runs.
    let mut world = BrowserWorld::new(page.clone(), context_ref);
    let executor = &self.bdd_executor;

    let mut output = String::new();
    let mut total_passed = 0usize;
    let mut total_failed = 0usize;
    let mut total_skipped = 0usize;

    for scenario in &scenarios {
      world.reset_scenario_state();
      let result = executor.run_scenario(&mut world, scenario).await;

      let status_icon = match result.status {
        ScenarioStatus::Passed => "[PASS]",
        ScenarioStatus::Failed => "[FAIL]",
        ScenarioStatus::Skipped => "[SKIP]",
        ScenarioStatus::Undefined => "[UNDEFINED]",
      };

      output.push_str(&format!(
        "{} {} ({:.1}s)\n",
        status_icon,
        result.scenario_name,
        result.duration.as_secs_f64()
      ));

      for step in &result.steps {
        let step_icon = match step.status {
          StepStatus::Passed => "  [ok]",
          StepStatus::Failed => "  [FAIL]",
          StepStatus::Skipped => "  [skip]",
          StepStatus::Undefined => "  [?]",
          StepStatus::Pending => "  [pending]",
        };
        output.push_str(&format!(
          "{} {}{} ({:.0}ms)\n",
          step_icon,
          step.keyword,
          step.text,
          step.duration.as_millis()
        ));
        if let Some(err) = &step.error {
          if step.status == StepStatus::Failed {
            output.push_str(&format!("         {}\n", err.lines().next().unwrap_or(err)));
          }
        }
      }

      match result.status {
        ScenarioStatus::Passed => total_passed += 1,
        ScenarioStatus::Failed => total_failed += 1,
        _ => total_skipped += 1,
      }

      output.push('\n');
    }

    output.push_str(&format!(
      "--- {} scenario(s): {} passed, {} failed, {} skipped ---\n",
      scenarios.len(),
      total_passed,
      total_failed,
      total_skipped,
    ));

    // Final snapshot of page state.
    let snap = self.snap(&page, s).await;
    output.push_str(&format!("\n{snap}"));

    Ok(CallToolResult::success(vec![Content::text(output)]))
  }

  #[tool(
    name = "run_step",
    description = "Execute a single BDD step on the current browser session using natural language. \
    Steps are matched against 100+ built-in definitions covering navigation, clicks, form filling, assertions, and more. \
    Use list_steps to discover available patterns. Returns the step result and an accessibility snapshot. \
    Prefer this over raw click/fill/navigate when you want natural-language automation with built-in assertions and waits. \
    For multi-step sequences, use run_scenario with inline Gherkin instead."
  )]
  async fn run_step(&self, Parameters(p): Parameters<RunStepParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_ref());
    let _guard = self.session_guard(s).await;

    let (page, context_ref) = Box::pin(self.page_and_context(s)).await?;
    let mut world = BrowserWorld::new(page.clone(), context_ref);
    let result = self.bdd_executor.run_step(&mut world, &p.step, None, None).await;

    let status_text = match result.status {
      StepStatus::Passed => "Passed",
      StepStatus::Failed => "Failed",
      StepStatus::Pending => "Pending (undefined step)",
      StepStatus::Skipped => "Skipped",
      StepStatus::Undefined => "Undefined",
    };

    let mut msg = format!("[{}] {} ({:.0}ms)", status_text, p.step, result.duration.as_millis());
    if let Some(err) = &result.error {
      msg.push_str(&format!("\n{err}"));
    }

    self.action_ok(&page, s, &msg).await
  }

  #[tool(
    name = "list_steps",
    description = "List all available BDD step definitions grouped by kind (Given/When/Then). \
    Each step shows its Cucumber expression pattern with parameter placeholders like {string}, {int}, {float}. \
    Use this to discover what steps are available before calling run_step or writing Gherkin for run_scenario. \
    Does not interact with the browser."
  )]
  async fn list_steps(&self, Parameters(p): Parameters<ListStepsParams>) -> Result<CallToolResult, ErrorData> {
    let steps = self.step_registry.steps();

    let kind_filter = p.kind.as_deref().map(|k| match k.to_lowercase().as_str() {
      "given" => ferridriver_bdd::step::StepKind::Given,
      "when" => ferridriver_bdd::step::StepKind::When,
      "then" => ferridriver_bdd::step::StepKind::Then,
      _ => ferridriver_bdd::step::StepKind::Step,
    });

    let filter_lower = p.filter.as_deref().map(str::to_lowercase);

    let mut output = String::new();

    for kind in &[
      ferridriver_bdd::step::StepKind::Given,
      ferridriver_bdd::step::StepKind::When,
      ferridriver_bdd::step::StepKind::Then,
      ferridriver_bdd::step::StepKind::Step,
    ] {
      if let Some(ref kf) = kind_filter {
        if kf != kind {
          continue;
        }
      }

      let filtered: Vec<_> = steps
        .iter()
        .filter(|s| s.kind == *kind)
        .filter(|s| {
          filter_lower
            .as_ref()
            .is_none_or(|f| s.expression.to_lowercase().contains(f))
        })
        .collect();

      if filtered.is_empty() {
        continue;
      }

      output.push_str(&format!("## {kind}\n\n"));
      for step in &filtered {
        output.push_str(&format!("- {}\n", step.expression));
      }
      output.push('\n');
    }

    if output.is_empty() {
      output = "No step definitions found matching the filter.".to_string();
    } else {
      output.insert_str(0, &format!("# BDD Step Definitions ({} total)\n\n", steps.len()));
    }

    Ok(CallToolResult::success(vec![Content::text(output)]))
  }
}
