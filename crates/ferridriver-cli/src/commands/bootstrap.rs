//! What every command needs before it can run.
//!
//! Resolving the config layer stack is not a command's job — it happens once,
//! the same way, whichever command was named. This module holds the parts of
//! that resolution too specific to belong in `ferridriver-config`: what a
//! subcommand counts as for extension purposes, which process-wide hooks a
//! `.ts` config layer needs installed before it can be evaluated, and how the
//! bundler and reporter registries get populated from the resolved document.

use std::path::Path;

use ferridriver_config::FerridriverConfig;

use crate::cli;

/// Re-resolve the layer stack with what the configured extension
/// packages contribute through `defineDefaults` underneath it.
///
/// A no-op when nothing is configured, which is every run that names no
/// extensions. The contributions are read under the host the subcommand
/// will run as: a package branches on `ferridriver.host`, so what it
/// asks for under `mcp` need not be what it asks for under `test`.
pub(crate) type ContributedDefaults = Vec<(String, serde_json::Value)>;

pub(crate) async fn read_extension_defaults(
  config: &FerridriverConfig,
  args: &cli::Cli,
) -> anyhow::Result<ContributedDefaults> {
  let specs = config.extension_specs();
  let Some(host) = extension_host_of(&args.command) else {
    return Ok(Vec::new());
  };
  if specs.is_empty() {
    return Ok(Vec::new());
  }
  let caps = ferridriver_script::ScriptCaps::resolve_with_commands(
    &config.scripting.allow_env,
    config.scripting.allow.commands.clone(),
  )
  .with_extension_policy(config.extensions.policy());
  let sidecars: Vec<String> = config.sidecars.iter().map(|s| s.name.clone()).collect();
  let env = ferridriver_script::RequirementEnv::from_caps(&caps, &sidecars);
  // A refusal by `[extensions.policy]` is never skippable, so it fails
  // the command here rather than being logged while the run carries on
  // as if the package had simply been absent.
  let defaults = ferridriver_script::extension_defaults(&specs, &env, &caps.extension_policy, host)
    .await
    .map_err(|e| anyhow::anyhow!("{}", e.message))?;
  for (package, _) in &defaults {
    tracing::debug!(target: "ferridriver::extensions", package, host = host.as_str(), "extension.defaults.applied");
  }
  Ok(defaults)
}

/// Install how a `.ts` / `.js` layer becomes a document.
///
/// Only when the stack actually has one. A configuration written
/// entirely in `.toml` / `.yaml` / `.json` never reaches this — no
/// bundler, no JavaScript runtime, nothing to pay for a feature it does
/// not use. That is a property of where the loader is called from, not
/// a flag anyone has to remember to check.
pub(crate) fn install_module_loader(config: &FerridriverConfig, startup: &mut ferridriver_config::Startup) {
  if !startup.has_module_layer() {
    return;
  }
  let caps = ferridriver_script::ScriptCaps::resolve_with_commands(
    &config.scripting.allow_env,
    config.scripting.allow.commands.clone(),
  )
  .with_extension_policy(config.extensions.policy());
  startup.set_module_loader(std::sync::Arc::new(move |path: &Path| {
    let path = path.to_path_buf();
    let caps = caps.clone();
    let cwd = std::env::current_dir()?;
    // The loader is called from a synchronous fold, and evaluating a
    // module is async — so it runs on its own runtime rather than
    // blocking the one the command is already on.
    std::thread::scope(|scope| {
      scope
        .spawn(move || {
          let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
          runtime
            .block_on(ferridriver_script::config_module::evaluate(&path, &cwd, caps))
            .map_err(|e| anyhow::anyhow!("{}", e.message))
        })
        .join()
        .map_err(|_| anyhow::anyhow!("config module evaluation panicked"))?
    })
  }));
}

/// The extension host a subcommand runs as, or `None` for one that
/// loads no extensions at all.
pub(crate) fn extension_host_of(command: &cli::Command) -> Option<ferridriver_script::ExtensionHost> {
  use ferridriver_script::ExtensionHost;
  match command {
    cli::Command::Mcp(_) => Some(ExtensionHost::Mcp),
    cli::Command::Bdd(_) => Some(ExtensionHost::Bdd),
    cli::Command::Test(_) | cli::Command::RustTest(_) => Some(ExtensionHost::Test),
    // `config` and `doctor` run nothing, but they exist to explain what
    // a run would see — so they read the contributions too, under the
    // neutral script host.
    cli::Command::Run(_) | cli::Command::Session(_) | cli::Command::Config(_) | cli::Command::Doctor(_) => {
      Some(ExtensionHost::Script)
    },
    cli::Command::Init(_)
    | cli::Command::Install(_)
    | cli::Command::Codegen(_)
    | cli::Command::Ext(_)
    | cli::Command::Trace(_)
    | cli::Command::MergeReports(_)
    | cli::Command::Upgrade(_)
    | cli::Command::Completions(_) => None,
  }
}

/// Compile every JS reporter the config names and install the factory
/// `create_reporters` consults for a name outside its built-in table.
/// Runs before the runner is built, because a reporter that cannot load
/// has to fail the command rather than the run.
pub(crate) async fn install_js_reporters(
  test: &ferridriver_test::config::TestConfig,
  caps: &ferridriver_script::ScriptCaps,
) -> anyhow::Result<()> {
  let cwd = std::env::current_dir()?;
  ferridriver_script::reporter::install(test, &cwd, caps)
    .await
    .map_err(|e| anyhow::anyhow!("{}", e.message))
}

/// Install the config's `[bundler]` section (import aliases, virtual
/// modules, resolution controls) plus `[test].tsconfig` into the
/// process-global slot every bundle path reads. Relative paths resolve
/// against the config file's directory, falling back to the cwd for a
/// default/in-memory config.
///
/// Runs before the subcommand dispatch because a session or compile
/// runtime built before the install would bundle against an empty
/// environment for its whole life.
pub(crate) fn install_bundler_env(config: &FerridriverConfig) {
  let base = config
    .source_dir
    .clone()
    .or_else(|| std::env::current_dir().ok())
    .unwrap_or_else(|| std::path::PathBuf::from("."));
  let env = ferridriver_script::bundle::BundlerEnv::from_config(&config.bundler, &base)
    .with_tsconfig(config.test.tsconfig.as_deref(), &base);
  ferridriver_script::bundle::set_bundler_env(env);
}

/// Install `[test].moduleAliases` plus any `--module-alias` flags into
/// the process-global slot the native module resolver, the throwaway
/// compile runtimes and the rolldown externals all read.
///
/// Runs at startup for EVERY subcommand, before anything bundles: the
/// table is read by the first resolver built, and a session created
/// before an alias arrives would keep resolving without it. The
/// resolved-config path re-states the same table later, which the
/// merge treats as the no-op it is.
pub(crate) fn install_module_aliases(
  test: &ferridriver_test::config::TestConfig,
  flags: &[String],
) -> anyhow::Result<()> {
  let mut table: Vec<(String, String)> = test
    .module_aliases
    .iter()
    .map(|(k, v)| (k.clone(), v.clone()))
    .collect();
  for spec in flags {
    let (from, to) = spec
      .split_once('=')
      .ok_or_else(|| anyhow::anyhow!("invalid --module-alias {spec:?} (expected <specifier>=<native module>)"))?;
    table.push((from.trim().to_string(), to.trim().to_string()));
  }
  ferridriver_script::set_module_aliases(table).map_err(|e| anyhow::anyhow!("{e}"))
}

/// `--module-alias` flags of whichever subcommand carries them, so the
/// merge can happen at startup rather than inside one subcommand's
/// config resolution.
pub(crate) fn module_alias_flags(command: &cli::Command) -> &[String] {
  match command {
    cli::Command::Test(args) => &args.module_alias,
    _ => &[],
  }
}
