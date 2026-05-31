//! Interactive recorder: launch headed browser, capture user actions, emit code.

use std::io::Write;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::chromium;
use crate::error::Result;
use crate::events::ExposedFn;
use crate::options::{BrowserContextOptions, LaunchOptions, ViewportOption};

use super::emitter::{CodeEmitter, GherkinEmitter, RustEmitter, TypeScriptEmitter};
use super::{Action, OutputLanguage};

/// Recorder JS injected into every page (survives navigations).
const RECORDER_JS: &str = include_str!("recorder.js");
const RECORDER_SUPPORT_JS: &str = include_str!("../injected/dist/recorder-support.min.js");

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

    // Launch headed Chromium and apply the viewport at the context
    // level — Playwright puts viewport on `BrowserContextOptions`,
    // not on `LaunchOptions`.
    let browser = chromium()
      .launch(LaunchOptions {
        headless: Some(false),
        ..Default::default()
      })
      .await?;

    let ctx_opts = self.options.viewport.map(|(w, h)| BrowserContextOptions {
      viewport: ViewportOption::Size {
        width: i64::from(w),
        height: i64::from(h),
      },
      ..Default::default()
    });
    let ctx = browser.new_context(ctx_opts);
    let page = Box::pin(ctx.new_page()).await?;
    page.goto(&self.options.url, None).await?;

    // Expose the action callback: JS -> Rust bridge.
    let emitter_cb = Arc::clone(&emitter);
    let output_cb = Arc::clone(&output);
    let callback: ExposedFn = Arc::new(move |args: Vec<serde_json::Value>| {
      let emitter_cb = Arc::clone(&emitter_cb);
      let output_cb = Arc::clone(&output_cb);
      Box::pin(async move {
        let json_str = args.first().and_then(|v| v.as_str()).unwrap_or("{}").to_string();
        if let Ok(action) = serde_json::from_str::<Action>(&json_str) {
          let code = emitter_cb.action(&action);
          if let Ok(mut out) = output_cb.try_lock() {
            let _ = out.write_all(code.as_bytes());
            let _ = out.flush();
          }
        }
        serde_json::Value::Null
      })
    });

    page.expose_function("__fdRecorderAction", callback).await?;

    // Inject recorder support + recorder JS (persists across navigations via add_init_script).
    page
      .add_init_script(
        crate::options::InitScriptSource::Source(RECORDER_SUPPORT_JS.into()),
        None,
      )
      .await?;
    page
      .add_init_script(crate::options::InitScriptSource::Source(RECORDER_JS.into()), None)
      .await?;
    // Also evaluate immediately for the current page.
    let _ = page.inner().evaluate(RECORDER_SUPPORT_JS).await;
    let _ = page.inner().evaluate(RECORDER_JS).await;

    eprintln!("Recording started. Interact with the browser.");
    eprintln!("Press Ctrl+C or close the browser to stop.\n");

    // Stop on browser close, Ctrl+C (SIGINT), or SIGTERM — finalize the
    // emitted script (footer) in every case so a non-interactive stop
    // (`kill`, editor, CI) still produces a complete, runnable file.
    #[cfg(unix)]
    {
      use tokio::signal::unix::{SignalKind, signal};
      let mut term =
        signal(SignalKind::terminate()).map_err(|e| crate::error::FerriError::backend(format!("signal: {e}")))?;
      tokio::select! {
        _ = page.wait_for_event("close", Some(86_400_000)) => {}
        _ = tokio::signal::ctrl_c() => { eprintln!("\nRecording stopped."); }
        _ = term.recv() => { eprintln!("\nRecording stopped."); }
      }
    }
    #[cfg(not(unix))]
    {
      tokio::select! {
        _ = page.wait_for_event("close", Some(86_400_000)) => {}
        _ = tokio::signal::ctrl_c() => { eprintln!("\nRecording stopped."); }
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
