//! `ferridriver test` — the TypeScript/JavaScript suite runner.

use ferridriver_config::FerridriverConfig;

use crate::cli;
use crate::commands::{script_setup, suite};

pub async fn run(config: FerridriverConfig, args: cli::TestRunArgs) -> anyhow::Result<()> {
  let caps = suite::caps(&config);
  ferridriver_testjs::set_test_script_caps(caps.clone());
  ferridriver_testjs::set_test_sidecars(script_setup::sidecar_specs(&config));

  let mut overrides = ferridriver_test::config::CliOverrides {
    test_files: args.files,
    grep: args.grep,
    grep_invert: args.grep_invert,
    tag: args.tag,
    retries: args.retries,
    timeout: args.timeout,
    last_failed: args.last_failed,
    only_changed: args.only_changed,
    fail_fast: args.runner.fail_fast,
    max_failures: args.max_failures,
    repeat_each: args.repeat_each,
    forbid_only: args.forbid_only,
    list_only: args.list,
    module_aliases: args.module_alias,
    ..Default::default()
  };
  suite::apply_shared(&mut overrides, &args.runner, &args.browser)?;

  let test_config = Box::pin(suite::resolve(config, &caps, args.runner.debug, &mut overrides)).await?;
  let exit_code = Box::pin(ferridriver_testjs::run_ts_tests_with(test_config, overrides)).await;
  suite::finish(exit_code);
  Ok(())
}
