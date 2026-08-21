//! `ferridriver trace` arguments.

use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

#[derive(Args)]
pub struct TraceArgs {
  #[command(flatten)]
  pub config: super::ConfigSource,

  #[command(subcommand)]
  pub command: TraceCommand,
}

#[derive(Subcommand)]
pub enum TraceCommand {
  /// Open a trace in the embedded viewer: actions, DOM snapshots, network,
  /// console, source. Serves the viewer from this binary — no download, no
  /// node, works offline.
  #[command(after_help = "Examples:\n  \
    ferridriver trace view\n  \
    ferridriver trace view test-results/login/trace.zip\n  \
    ferridriver trace view --port 9323 --no-open")]
  View(TraceViewArgs),

  /// Print a trace in the terminal: the call tree with timings, what failed
  /// and what it was waiting for, plus console and network summaries.
  #[command(after_help = "Examples:\n  \
    ferridriver trace show\n  \
    ferridriver trace show --errors\n  \
    ferridriver trace show --hide network --hide console --limit 50")]
  Show(TraceShowArgs),

  /// List the traces under a directory, newest first.
  #[command(after_help = "Examples:\n  \
    ferridriver trace ls\n  \
    ferridriver trace ls test-results/ --format json")]
  Ls(TraceLsArgs),
}

#[derive(Args)]
pub struct TraceViewArgs {
  /// Trace to open: a `trace.zip`, a directory of trace files, or an
  /// `http(s)` URL. Defaults to the newest trace under the test output
  /// directory.
  pub trace: Option<String>,

  /// Port to serve the viewer on (an ephemeral one by default).
  #[arg(long)]
  pub port: Option<u16>,

  /// Address to bind. Anything other than loopback exposes the traces on
  /// this machine to the network.
  #[arg(long, default_value = "127.0.0.1")]
  pub host: String,

  /// Print the URL and keep serving instead of opening a browser.
  #[arg(long)]
  pub no_open: bool,
}

#[derive(Args)]
pub struct TraceShowArgs {
  /// Trace to print: a `trace.zip` or a directory of trace files. Defaults
  /// to the newest trace under the test output directory.
  pub trace: Option<PathBuf>,

  /// Only the failures: failing calls, console errors, failed requests.
  #[arg(long)]
  pub errors: bool,

  /// Stop after this many calls.
  #[arg(long)]
  pub limit: Option<usize>,

  /// Sections to leave out. Repeatable: `--hide logs --hide network`.
  #[arg(long = "hide", value_enum)]
  pub hide: Vec<TraceSection>,
}

/// A section of a printed trace.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TraceSection {
  /// Per-call log lines (what a retrying call was waiting for).
  Logs,
  /// Console messages from the page.
  Console,
  /// Network requests.
  Network,
}

#[derive(Args)]
pub struct TraceLsArgs {
  /// Directory to scan. Defaults to the test output directory.
  pub dir: Option<PathBuf>,
}
