//! NAPI binding for the interactive code recorder (codegen).

use napi::Result;
use napi_derive::napi;

/// Codegen configuration from TypeScript.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct CodegenConfig {
  /// URL to open in the browser.
  pub url: String,
  /// Output language: "rust" (default), "typescript"/"ts", "gherkin"/"bdd".
  pub language: Option<String>,
  /// Write to file instead of stdout.
  pub output_file: Option<String>,
  /// Viewport width.
  pub viewport_width: Option<i32>,
  /// Viewport height.
  pub viewport_height: Option<i32>,
}

/// Interactive code recorder.
#[napi]
pub struct Codegen;

#[napi]
impl Codegen {
  /// Run the interactive recorder.
  ///
  /// Opens a headed browser, navigates to the URL, and records user
  /// interactions as test code. Blocks until the browser is closed or
  /// the process is interrupted.
  #[napi]
  pub async fn run(config: CodegenConfig) -> Result<()> {
    let viewport = match (config.viewport_width, config.viewport_height) {
      (Some(w), Some(h)) => Some((w as u32, h as u32)),
      _ => None,
    };

    let options = ferridriver::codegen::recorder::RecorderOptions {
      url: config.url,
      language: ferridriver::codegen::OutputLanguage::from_str(
        config.language.as_deref().unwrap_or("rust"),
      ),
      output_file: config.output_file,
      viewport,
    };

    ferridriver::codegen::recorder::Recorder::new(options)
      .start()
      .await
      .map_err(napi::Error::from_reason)
  }
}
