//! `ferridriver ext` arguments.

use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Args)]
pub struct ExtArgs {
  #[command(flatten)]
  pub config: super::ConfigSource,

  #[command(subcommand)]
  pub command: ExtCommand,
}

#[derive(Subcommand)]
pub enum ExtCommand {
  /// Verify extensions once: resolve, type-check, load, and report every
  /// tool, capability, unmet requirement and error. Exits non-zero when
  /// something is wrong, so it works as a pre-commit / CI gate.
  #[command(after_help = "Examples:\n  \
    ferridriver ext check\n  \
    ferridriver ext check ./my-extension --no-typecheck\n  \
    ferridriver ext check --format json")]
  Check(ExtCheckArgs),

  /// The authoring loop: `check` re-run on every save.
  #[command(after_help = "Examples:\n  ferridriver ext dev ./my-extension")]
  Dev(ExtCheckArgs),

  /// Write the `@ferridriver/extension` (and `@ferridriver/test`) type
  /// declarations this binary type-checks against, so an editor resolves
  /// the same surface. No npm install needed.
  #[command(after_help = "Examples:\n  \
    ferridriver ext types\n  \
    ferridriver ext types -o ./types")]
  Types(ExtTypesArgs),
}

#[derive(Args)]
pub struct ExtCheckArgs {
  /// Extension files, directories, packages, or package specifiers.
  /// Defaults to the `extensions` list from the resolved config.
  pub paths: Vec<String>,

  /// Re-run whenever a file under an extension's root changes. Implied by
  /// `ext dev`.
  #[arg(long, short = 'w')]
  pub watch: bool,

  /// Skip the TypeScript pass (only resolve + load).
  #[arg(long)]
  pub no_typecheck: bool,
}

#[derive(Args)]
pub struct ExtTypesArgs {
  /// Directory to write `@ferridriver/extension/` and
  /// `@ferridriver/test/` into. Defaults to `./node_modules`, which is
  /// where TypeScript already looks.
  #[arg(long, short = 'o')]
  pub out: Option<PathBuf>,
}
