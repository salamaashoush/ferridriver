//! `ferridriver bdd` arguments.

use clap::Args;

use super::browser::BrowserArgs;
use super::runner::RunnerArgs;

// Independent bool flags from `clap` parse — grouping into enums adds
// no value; each flag has its own --foo.
#[allow(clippy::struct_excessive_bools)]
#[derive(Args)]
pub struct BddArgs {
  /// Feature file globs. Overrides `[bdd].features` from config.
  pub features: Vec<String>,

  /// Tag filter expression, e.g. `@smoke and not @wip`.
  #[arg(long, help_heading = "Selection")]
  pub tags: Option<String>,

  /// Parse and report scenarios without executing steps.
  #[arg(long, help_heading = "Selection")]
  pub dry_run: bool,

  /// Treat undefined or pending steps as failures (default).
  #[arg(long, overrides_with = "no_strict", help_heading = "Gherkin")]
  pub strict: bool,

  /// Report undefined or pending steps without failing the run.
  #[arg(long = "no-strict", overrides_with = "strict", help_heading = "Gherkin")]
  pub no_strict: bool,

  /// Per-step timeout in milliseconds.
  #[arg(long, help_heading = "Gherkin")]
  pub step_timeout: Option<u64>,

  /// Scenario execution order: `defined`, `random`, or `random:<seed>`.
  #[arg(long, help_heading = "Gherkin")]
  pub order: Option<String>,

  /// Gherkin keyword language (e.g. `en`, `de`, `fr`).
  #[arg(long, help_heading = "Gherkin")]
  pub language: Option<String>,

  /// JavaScript step-definition file globs, e.g.
  /// `--steps 'steps/**/*.js'`. May be repeated. Overrides
  /// `[test].steps` from config. Defaults to `steps/**/*.js` and
  /// `step_definitions/**/*.js` when omitted.
  #[arg(long, help_heading = "Gherkin")]
  pub steps: Vec<String>,

  /// Cucumber world parameters as a JSON object, exposed to every
  /// scenario as `this.parameters`. Overrides `[test].worldParameters`.
  #[arg(long, help_heading = "Gherkin")]
  pub world_parameters: Option<String>,

  #[command(flatten)]
  pub runner: RunnerArgs,

  #[command(flatten)]
  pub browser: BrowserArgs,
}

impl BddArgs {
  /// `--strict` / `--no-strict`, or `None` when neither was passed and the
  /// config decides.
  #[must_use]
  pub fn strict(&self) -> Option<bool> {
    match (self.strict, self.no_strict) {
      (true, _) => Some(true),
      (_, true) => Some(false),
      _ => None,
    }
  }
}
