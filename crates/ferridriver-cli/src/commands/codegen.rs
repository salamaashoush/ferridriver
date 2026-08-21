//! `ferridriver codegen` — record interactions and emit them as a test.

use ferridriver::codegen::OutputLanguage;
use ferridriver::codegen::recorder::{Recorder, RecorderOptions};

use crate::cli;
use crate::ui;

/// Launch the interactive recorder: open a headed browser, capture the user's
/// interactions, and emit a runnable script (TypeScript by default) to stdout
/// or `--output`. The emitted script runs standalone via `ferridriver run`
/// and replays on a live session via the MCP `run_script` tool.
pub async fn run(args: cli::CodegenArgs) -> anyhow::Result<()> {
  let url = args.url.unwrap_or_else(|| "about:blank".to_string());
  ui::note(&format!("recording from {}", ui::url(&url)));
  ui::note("interact with the page, then close the browser to finish");
  let output = args.output.clone();
  let options = RecorderOptions {
    url,
    language: OutputLanguage::parse_cli(&args.language),
    output_file: args.output.as_deref().map(|p| p.to_string_lossy().into_owned()),
    viewport: None,
  };
  Recorder::new(options)
    .start()
    .await
    .map_err(|e| anyhow::anyhow!("codegen: {e}"))?;
  if let Some(path) = output {
    ui::say(&ui::success(&format!(
      "wrote {}",
      ui::path(&path.display().to_string())
    )));
    ui::next_steps(&[("run it", format!("ferridriver run {}", path.display()))]);
  }
  Ok(())
}
