//! Interactive recorder: launch headed browser, capture user actions, emit code.

use std::io::Write;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::Browser;
use crate::error::Result;
use crate::events::ExposedFn;
use crate::options::LaunchOptions;

use super::emitter::{CodeEmitter, GherkinEmitter, RustEmitter, TypeScriptEmitter};
use super::{Action, OutputLanguage};

/// Recorder JS injected into every page (survives navigations).
const RECORDER_JS: &str = include_str!("recorder.js");

/// Options for the interactive recorder.
pub struct RecorderOptions {
  pub url: String,
  pub language: OutputLanguage,
  pub output_file: Option<String>,
  pub viewport: Option<(u32, u32)>,
}

/// Interactive code recorder.
pub struct Recorder {
  options: RecorderOptions,
}

impl Recorder {
  #[must_use]
  pub fn new(options: RecorderOptions) -> Self {
    Self { options }
  }

  /// Run the recorder: launch browser, record, emit code until browser closes or Ctrl+C.
  ///
  /// # Errors
  ///
  /// Returns an error if browser launch or navigation fails.
  pub async fn start(&self) -> Result<()> {
    let emitter: Arc<dyn CodeEmitter> = match self.options.language {
      OutputLanguage::Rust => Arc::new(RustEmitter),
      OutputLanguage::TypeScript => Arc::new(TypeScriptEmitter),
      OutputLanguage::Gherkin => Arc::new(GherkinEmitter::new()),
    };

    // Output target: file or stdout.
    let output: Arc<Mutex<Box<dyn Write + Send>>> = if let Some(ref path) = self.options.output_file {
      let file = std::fs::File::create(path).map_err(|e| format!("cannot create output file {path}: {e}"))?;
      Arc::new(Mutex::new(Box::new(std::io::BufWriter::new(file))))
    } else {
      Arc::new(Mutex::new(Box::new(std::io::stdout())))
    };

    // Emit header.
    {
      let header = emitter.header(&self.options.url);
      let mut out = output.lock().await;
      let _ = out.write_all(header.as_bytes());
      let _ = out.flush();
    }

    // Launch headed browser.
    let viewport = self.options.viewport.map(|(w, h)| crate::options::ViewportConfig {
      width: i64::from(w),
      height: i64::from(h),
      ..Default::default()
    });
    let browser = Browser::launch(LaunchOptions {
      headless: false,
      viewport,
      ..Default::default()
    })
    .await?;

    let ctx = browser.new_context();
    let page = Box::pin(ctx.new_page()).await?;
    page.goto(&self.options.url, None).await?;

    // Expose the action callback: JS -> Rust bridge.
    let emitter_cb = Arc::clone(&emitter);
    let output_cb = Arc::clone(&output);
    let callback: ExposedFn = Arc::new(move |args: Vec<serde_json::Value>| {
      let json_str = args.first().and_then(|v| v.as_str()).unwrap_or("{}");
      if let Ok(action) = serde_json::from_str::<Action>(json_str) {
        let code = emitter_cb.action(&action);
        // Synchronous lock — ExposedFn is not async, but this is fine for stdout/file writes.
        if let Ok(mut out) = output_cb.try_lock() {
          let _ = out.write_all(code.as_bytes());
          let _ = out.flush();
        }
      }
      serde_json::Value::Null
    });

    page.expose_function("__fdRecorderAction", callback).await?;

    // Inject recorder JS (persists across navigations via add_init_script).
    page.add_init_script(RECORDER_JS).await?;
    // Also evaluate immediately for the current page.
    let _ = page.evaluate(RECORDER_JS).await;

    eprintln!("Recording started. Interact with the browser.");
    eprintln!("Press Ctrl+C or close the browser to stop.\n");

    // Wait for browser close or Ctrl+C.
    tokio::select! {
      _ = page.wait_for_event("close", Some(86_400_000)) => {
        // Browser/page closed by user.
      }
      _ = tokio::signal::ctrl_c() => {
        eprintln!("\nRecording stopped.");
      }
    }

    // Emit footer.
    {
      let footer = emitter.footer();
      let mut out = output.lock().await;
      let _ = out.write_all(footer.as_bytes());
      let _ = out.flush();
    }

    Ok(())
  }
}
