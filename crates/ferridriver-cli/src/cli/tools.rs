//! Arguments for the commands that do one job and exit: scaffolding a
//! project, serving MCP, installing browsers, recording a script, merging
//! shard reports, emitting completions.

use std::path::PathBuf;

use clap::Args;

use super::browser::{BrowserArgs, TransportArgs};

#[derive(Args)]
pub struct InitArgs {
  /// Directory to scaffold. Defaults to the current one.
  pub dir: Option<PathBuf>,

  /// Syntax to write the config in. Named `--config-format` rather than
  /// `--format`, which is the global output-format flag.
  #[arg(long = "config-format", value_name = "EXT", default_value = "toml", value_parser = ["toml", "yaml", "json"])]
  pub config_format: String,

  /// Also scaffold a feature file and a step file for `ferridriver bdd`.
  #[arg(long)]
  pub bdd: bool,

  /// Overwrite files that already exist. Without it, anything present is
  /// left alone and reported as skipped.
  #[arg(long)]
  pub force: bool,
}

#[derive(Args)]
pub struct McpArgs {
  #[command(flatten)]
  pub browser: BrowserArgs,

  #[command(flatten)]
  pub transport: TransportArgs,
}

#[derive(Args)]
pub struct InstallArgs {
  /// Browsers to install: `chromium`, `chromium-headless-shell`,
  /// `firefox`, `webkit`. Defaults to `chromium` when omitted.
  pub browsers: Vec<String>,

  /// Also install required system libraries (Linux only; uses the
  /// platform package manager and may require sudo).
  #[arg(long)]
  pub with_deps: bool,
}

#[derive(Args)]
pub struct CodegenArgs {
  /// URL to open in the codegen browser.
  pub url: Option<String>,

  /// Output file for generated test code.
  #[arg(short, long)]
  pub output: Option<PathBuf>,

  /// Output language: `ts` (runnable script, default), `rust`
  /// (`#[ferritest]`), or `gherkin` (`.feature`).
  #[arg(long, default_value = "ts", value_parser = ["ts", "rust", "gherkin"])]
  pub language: String,

  #[command(flatten)]
  pub browser: BrowserArgs,
}

#[derive(Args)]
pub struct MergeReportsArgs {
  /// Directory holding the shards' blob zips, or the zips themselves.
  #[arg(default_value = ".")]
  pub inputs: Vec<PathBuf>,

  /// Reporter to produce from the merged stream, repeatable. Defaults to
  /// `list` plus whatever the config declares.
  #[arg(long)]
  pub reporter: Vec<String>,

  /// Where the merged report's files are written. Defaults to the
  /// config's `outputDir`.
  #[arg(long)]
  pub output_dir: Option<PathBuf>,
}

// Independent flags with no relationship to each other: grouping them into
// an enum would model a state machine that does not exist and would change
// the CLI surface to satisfy a lint about the struct behind it.
#[allow(clippy::struct_excessive_bools)]
#[derive(Args)]
pub struct UpgradeArgs {
  /// Follow the canary channel: the rolling prerelease built from every
  /// push to main. A canary build already follows it; this is how a stable
  /// build opts in.
  #[arg(long, conflicts_with = "stable")]
  pub canary: bool,

  /// Follow the stable channel. Only needed to move a canary build back
  /// onto the released line.
  #[arg(long, conflicts_with = "canary")]
  pub stable: bool,

  /// Report what is available and change nothing.
  #[arg(long)]
  pub check: bool,

  /// Install even when the running version is already current — for
  /// repairing a damaged install, or moving between channels at the same
  /// version.
  #[arg(long)]
  pub force: bool,

  /// Install this exact release tag instead of the channel's newest, e.g.
  /// `v0.4.0`. Downgrades are allowed.
  #[arg(long, value_name = "TAG")]
  pub tag: Option<String>,
}

impl UpgradeArgs {
  /// The channel this run follows: what the flags say, or the channel this
  /// binary was built for. A canary that silently upgraded itself onto
  /// stable would be a one-way door nobody asked for.
  #[must_use]
  pub fn channel(&self) -> crate::commands::upgrade::Channel {
    use crate::commands::upgrade::Channel;
    match (self.canary, self.stable) {
      (true, _) => Channel::Canary,
      (_, true) => Channel::Stable,
      _ if crate::build_info::is_canary() => Channel::Canary,
      _ => Channel::Stable,
    }
  }
}

#[derive(Args)]
pub struct CompletionsArgs {
  /// Shell to generate for. Detected from `$SHELL` when omitted.
  #[arg(value_enum)]
  pub shell: Option<clap_complete::Shell>,
}
