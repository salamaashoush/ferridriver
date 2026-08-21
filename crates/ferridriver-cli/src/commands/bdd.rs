//! `ferridriver bdd` — the Gherkin suite runner.

use ferridriver_config::FerridriverConfig;

use crate::cli;
use crate::commands::{script_setup, suite};

pub async fn run(config: FerridriverConfig, args: cli::BddArgs) -> anyhow::Result<()> {
  let caps = suite::caps(&config);
  ferridriver_bdd::js::set_bdd_script_caps(caps.clone());
  ferridriver_bdd::js::set_bdd_sidecars(script_setup::sidecar_specs(&config));

  let strict = args.strict();
  let mut overrides = ferridriver_test::config::CliOverrides {
    bdd_tags: args.tags,
    bdd_dry_run: args.dry_run,
    bdd_fail_fast: args.runner.fail_fast,
    bdd_strict: strict,
    bdd_step_timeout: args.step_timeout,
    bdd_order: args.order,
    bdd_language: args.language,
    bdd_steps: args.steps,
    world_parameters: args.world_parameters,
    ..Default::default()
  };
  suite::apply_shared(&mut overrides, &args.runner, &args.browser)?;

  let mut test_config = Box::pin(suite::resolve(config, &caps, args.runner.debug, &mut overrides)).await?;
  // CLI-supplied feature globs override the [test].features list when provided.
  if !args.features.is_empty() {
    test_config.features = args.features;
  }

  let exit_code = Box::pin(ferridriver_bdd::run_bdd_with(test_config, overrides)).await;
  suite::finish(exit_code);
  Ok(())
}
