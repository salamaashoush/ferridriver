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
  pub config: super::ConfigSource,

  #[command(flatten)]
  pub browser: BrowserArgs,

  #[command(flatten)]
  pub transport: TransportArgs,
}

#[derive(Args)]
pub struct InstallArgs {
  /// Browsers to install: `chromium`, `chromium-headless-shell`,
  /// `firefox`, `webkit`, `safari`, `android`, `android-driver`, `ios`, or `axe`.
  /// Defaults to `chromium`. `android-driver` is the pair of APKs the
  /// Android device API installs on a device for its selector actions.
  pub browsers: Vec<String>,

  /// Also install required system libraries (Linux only; uses the
  /// platform package manager and may require sudo).
  #[arg(long)]
  pub with_deps: bool,

  /// Keep the older versions of a browser this install replaces. Without
  /// it they are removed from the cache: a launch takes the newest
  /// version there, so nothing would run them again.
  #[arg(long)]
  pub no_remove: bool,

  #[arg(long)]
  pub accept_licenses: bool,

  #[arg(long, default_value_t = 35)]
  pub android_api_level: u32,

  #[arg(long)]
  pub android_sdk: Option<PathBuf>,
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

  /// What TypeScript output is: `script`, a file `ferridriver run`
  /// executes, or `test`, an `@ferridriver/test` spec `ferridriver test`
  /// runs. Rust and Gherkin output are tests already.
  #[arg(long, default_value = "script", value_parser = ["script", "test"])]
  pub target: String,

  /// The test's title in a `--target test` spec.
  #[arg(long, value_name = "TITLE")]
  pub title: Option<String>,

  /// Record on a live session instead of launching a browser: whatever
  /// drives the session's page, a person at its headed window or input
  /// relayed to a headless one, is recorded. The URL, when given, is opened
  /// first. Type `visible`, `text` or `value` and Enter, then click an
  /// element, to record an assertion about it; `url` or `title` asserts the
  /// page itself. `stop`, Ctrl+C or SIGTERM ends the recording.
  #[arg(long, value_name = "SESSION", conflicts_with = "pick_locator")]
  pub attach: Option<String>,

  /// Browser context within the attached session.
  #[arg(long, requires = "attach")]
  pub context: Option<String>,

  /// Pick one element instead of recording: open the page, wait for a
  /// click, and print that element's selector. Answers "what do I call
  /// this" rather than "what did I just do", so nothing is recorded and
  /// `--language` and `--output` do not apply.
  #[arg(long)]
  pub pick_locator: bool,

  #[command(flatten)]
  pub browser: BrowserArgs,
}

#[derive(Args)]
pub struct MergeReportsArgs {
  #[command(flatten)]
  pub config: super::ConfigSource,

  /// Directory holding the shards' blob zips (and `stream` reporter
  /// `.jsonl` files), or those files themselves.
  #[arg(default_value = ".")]
  pub inputs: Vec<PathBuf>,

  /// Reporter to produce from the merged stream, repeatable, optionally
  /// with a target (`junit=merged.xml`); a path names a JS/TS reporter.
  /// Defaults to `list` plus whatever the config declares.
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
