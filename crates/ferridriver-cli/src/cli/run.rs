//! `ferridriver run` arguments.

use std::path::PathBuf;

use clap::Args;

// `headed`, `trace` and `report` are independent command-line flags with no
// relationship to each other -- grouping them into an enum would model a state
// machine that does not exist, and would change the CLI surface to satisfy a
// lint about the struct behind it.
#[allow(clippy::struct_excessive_bools)]
#[derive(Args)]
pub struct RunArgs {
  /// Script file, or `-` to read source from stdin. Omit when using
  /// `--eval`. A `.ts`/`.tsx` file, or any source with top-level
  /// `import`/`export`, is rolldown-bundled + transpiled + run as an ES
  /// module (its `default` export is the result). Plain `.js` scripts run
  /// as before, where top-level `return <value>` is the result.
  pub script: Option<String>,

  /// Inline script source (alternative to a file / stdin). Treated as an
  /// ES module when it contains top-level `import`/`export`.
  #[arg(short = 'e', long = "eval", conflicts_with = "script", help_heading = "Source")]
  pub eval: Option<String>,

  /// Bind `page` / `context` / `browser` from a configured `[browser]`
  /// instance, launching or attaching exactly as the MCP server and the test
  /// runner do for the same name -- including its args and discover commands.
  ///
  /// Without it a script owns its own browser via `chromium()` / `firefox()` /
  /// `webkit()` and no browser is started, so a script that never opens one
  /// costs nothing.
  #[arg(long, conflicts_with = "session", help_heading = "Browser")]
  pub instance: Option<String>,

  /// Show the browser window, overriding whatever `headless` the instance or
  /// the `[browser]` section sets. Only meaningful with `--instance`.
  #[arg(long, requires = "instance", help_heading = "Browser")]
  pub headed: bool,

  /// Per-script wall-clock timeout in milliseconds.
  #[arg(long, help_heading = "Run")]
  pub timeout_ms: Option<u64>,

  /// Log every Playwright-level action (`page.*`, `locator.*`, `expect.*`)
  /// to stderr as it starts and finishes, with its parameters, call log and
  /// duration. Independent of `context.tracing.start()` — nothing is
  /// recorded to a trace zip. Not available with `--session`, where the
  /// actions run in the host process.
  #[arg(long, help_heading = "Output")]
  pub trace: bool,

  /// Extension file(s), directory(ies), or ESM package specifiers to
  /// load, exposing their `tool` registrations to scripts as `tools.*`.
  /// Repeatable; merged with the `extensions` list from `ferridriver.toml`.
  /// Not accepted with `--session`, where the host owns the extension set.
  /// Rejected at run time rather than by clap, so the error can point at
  /// `session open` instead of just naming the conflict.
  #[arg(long = "extension", help_heading = "Browser")]
  pub extensions: Vec<String>,

  /// Run against a live session instead of launching a browser: the script
  /// gets that session's `page` / `context` / `request` globals, and its
  /// state (cookies, storage, `vars`, open pages) persists between runs.
  /// Open one with `ferridriver session open <id>`.
  #[arg(long, short = 's', help_heading = "Browser")]
  pub session: Option<String>,

  /// Browser context within the session (the `:context` half of a session
  /// key). Defaults to the session's default context.
  #[arg(long, requires = "session", help_heading = "Browser")]
  pub context: Option<String>,

  /// Emit the source that reproduces every action the script performs, in
  /// `ts` (default), `rust`, or `gherkin`. Lines go to stderr as they happen,
  /// or into the `--format json` document; `--code-out` writes a runnable file.
  #[arg(long, num_args = 0..=1, default_missing_value = "ts", value_name = "LANGUAGE", help_heading = "Output")]
  pub code: Option<String>,

  /// Write the generated source to this file, wrapped in the language's
  /// test scaffolding. Implies `--code`.
  #[arg(long, value_name = "FILE", help_heading = "Output")]
  pub code_out: Option<PathBuf>,

  /// After the run, print the agent-facing response: the result, the source
  /// reproducing what ran (with `--code`), and the page the session is left
  /// on, as `### `-titled sections. With `--format json` the same parts are folded
  /// into the result document under `report` instead of printed.
  ///
  /// The page section needs a page this process can read, so it appears for
  /// `--session` runs; a local run's script owns its own browser and this
  /// process never holds a handle to it.
  #[arg(long, help_heading = "Output")]
  pub report: bool,

  /// Positional args exposed to the script as the `args` global
  /// (strings). Pass after `--`.
  #[arg(last = true)]
  pub script_args: Vec<String>,
}
