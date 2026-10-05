//! `ferridriver session` arguments.

use clap::{Args, Subcommand};

use super::browser::BrowserArgs;

#[derive(Args)]
pub struct SessionArgs {
  #[command(flatten)]
  pub config: super::ConfigSource,

  #[command(subcommand)]
  pub command: SessionCommand,
}

#[derive(Subcommand)]
pub enum SessionCommand {
  /// Provision a browser or device, bind it under `id`, and serve it in the background.
  /// Spawns a detached host process and returns once the session is live.
  #[command(after_help = "Examples:\n  \
    ferridriver session open dev\n  \
    ferridriver session open staging https://example.com --headed\n  \
    ferridriver session open dev --extension ./tools")]
  Open(SessionOpenArgs),

  /// Internal: run the long-lived session host in the foreground (launch +
  /// bind + serve until killed). `open` spawns this detached; not meant to be
  /// invoked directly.
  #[command(hide = true)]
  Host(SessionHostArgs),

  /// Attach to a live session: connect and print its current snapshot.
  #[command(after_help = "Examples:\n  ferridriver session attach dev")]
  Attach(SessionTargetArgs),

  /// List all live sessions discoverable in the registry.
  #[command(
    visible_alias = "ls",
    after_help = "Examples:\n  \
    ferridriver session list\n  \
    ferridriver session list --format json"
  )]
  List(SessionListArgs),

  /// Close the browser and wait for provider cleanup before retiring its session.
  Close(SessionTargetArgs),

  /// Close every live session.
  CloseAll,
}

#[derive(Args)]
pub struct SessionOpenArgs {
  /// Session id to publish the browser under.
  pub id: String,

  /// URL to open in the session's first page (defaults to `about:blank`).
  pub url: Option<String>,

  /// Configured browser or device instance to host.
  #[arg(long)]
  pub instance: Option<String>,

  /// Extension file(s), directory(ies), or ESM package specifiers the
  /// session's scripts get as `tools.*`. Repeatable; merged with the
  /// `extensions` list from `ferridriver.toml`. The host loads these once,
  /// so every `ferridriver run --session <id>` sees them.
  #[arg(long = "extension")]
  pub extensions: Vec<String>,

  #[command(flatten)]
  pub browser: BrowserArgs,
}

#[derive(Args)]
pub struct SessionHostArgs {
  /// Session id to publish the browser under.
  pub id: String,

  /// URL to open in the session's first page.
  pub url: Option<String>,

  /// Configured browser or device instance to host.
  #[arg(long)]
  pub instance: Option<String>,

  #[arg(long, hide = true)]
  pub startup_error: Option<std::path::PathBuf>,

  /// Extensions to load for this session's scripts (see `session open`).
  #[arg(long = "extension")]
  pub extensions: Vec<String>,

  #[command(flatten)]
  pub browser: BrowserArgs,
}

#[derive(Args)]
pub struct SessionTargetArgs {
  /// Session id.
  pub id: String,
}

#[derive(Args)]
pub struct SessionListArgs;
