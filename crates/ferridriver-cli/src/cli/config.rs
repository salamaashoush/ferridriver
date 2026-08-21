//! `ferridriver config` and `ferridriver doctor` arguments.

use clap::Args;

use super::browser::BrowserArgs;

#[derive(Args)]
pub struct ConfigArgs {
  #[command(flatten)]
  pub config: super::ConfigSource,

  /// Print only the merged document, without the layer/provenance report.
  #[arg(long)]
  pub resolved: bool,

  /// Browser flags, so the report shows the SAME effective values a
  /// `ferridriver mcp` with these flags would run with.
  #[command(flatten)]
  pub browser: BrowserArgs,
}

#[derive(Args)]
pub struct DoctorArgs {
  #[command(flatten)]
  pub config: super::ConfigSource,

  /// Also run each configured instance's args/discover command. Off by
  /// default because those shell out (and a discover command may block
  /// while it waits for a browser).
  #[arg(long)]
  pub instances: bool,
}
