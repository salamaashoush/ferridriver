//! `ferridriver browser` arguments.

use clap::{Args, Subcommand};

#[derive(Args)]
pub struct BrowserCommandArgs {
  #[command(subcommand)]
  pub command: BrowserCommand,
}

#[derive(Subcommand)]
pub enum BrowserCommand {
  /// Launch Chromium and serve its `DevTools` endpoint to other processes.
  ///
  /// Prints the WebSocket URL a CDP client dials (`connectOverCDP`,
  /// `ferridriver test --connect`) on stdout, then serves until it is sent
  /// SIGTERM or SIGINT, or the browser exits, closing the browser on the
  /// way out.
  #[command(after_help = "Examples:\n  \
    ferridriver browser serve\n  \
    ferridriver browser serve --host 0.0.0.0 --port 9300 --token \"$GRID_TOKEN\"\n  \
    ferridriver test --connect ws://grid-worker:9300/<path>?token=...")]
  Serve(BrowserServeArgs),
}

#[derive(Args)]
pub struct BrowserServeArgs {
  /// Address to listen on. Anything but loopback gets a token, generated
  /// when `--token` is not given.
  #[arg(long, default_value = "127.0.0.1")]
  pub host: std::net::IpAddr,

  /// Port to listen on; `0` picks a free one.
  #[arg(long, default_value_t = 0)]
  pub port: u16,

  /// A secret every client must present, as `Authorization: Bearer <token>`
  /// or a `token` query parameter. The printed URL carries it. Read from
  /// `FERRIDRIVER_BROWSER_TOKEN` when not given, which keeps it out of `ps`.
  #[arg(long)]
  pub token: Option<String>,

  /// Path the endpoint is served at. Defaults to an unguessable string.
  #[arg(long)]
  pub ws_path: Option<String>,

  /// Show the browser window. Served browsers are headless by default.
  #[arg(long)]
  pub headed: bool,

  /// Chromium binary to launch instead of the installed one.
  #[arg(long)]
  pub executable_path: Option<String>,

  /// Extra Chromium switch, repeatable (`--arg=--lang=de`).
  #[arg(long = "arg", allow_hyphen_values = true)]
  pub args: Vec<String>,
}
