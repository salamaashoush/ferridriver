//! Headed interactive recorder (`ferridriver codegen`) built on [`super::Recorder`].

use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};

use ferridriver::chromium;
use ferridriver::error::{FerriError, Result};
use ferridriver::options::{BrowserContextOptions, LaunchOptions, ViewportOption};
use ferridriver::page::Page;

use super::{Recorder, RecorderEvent};
use crate::{OutputLanguage, Scaffold};

/// Options for the interactive recorder.
pub struct RecorderOptions {
  pub url: String,
  pub language: OutputLanguage,
  pub scaffold: Scaffold,
  pub output_file: Option<String>,
  pub viewport: Option<(u32, u32)>,
}

/// `ferridriver codegen`: a headed Chromium whose interactions are written
/// out as code while they happen.
pub struct HeadedRecorder {
  options: RecorderOptions,
}

impl HeadedRecorder {
  #[must_use]
  pub fn new(options: RecorderOptions) -> Self {
    Self { options }
  }

  async fn open_page(&self) -> Result<(ferridriver::Browser, Arc<Page>)> {
    let browser = chromium()
      .launch(LaunchOptions {
        headless: Some(false),
        ..Default::default()
      })
      .await?;
    // Playwright puts viewport on `BrowserContextOptions`, not on
    // `LaunchOptions`.
    let ctx_opts = self.options.viewport.map(|(w, h)| BrowserContextOptions {
      viewport: ViewportOption::Size {
        width: i64::from(w),
        height: i64::from(h),
      },
      ..Default::default()
    });
    let ctx = browser.new_context().maybe_options(ctx_opts).await?;
    let page = Box::pin(ctx.new_page()).await?;
    page.goto(&self.options.url).await?;
    Ok((browser, page))
  }

  /// Open the page and hand back the selector for whatever the reader
  /// clicks.
  ///
  /// The recorder answers "what did I just do"; this answers "what do I
  /// call that", which is the question left over when a selector has to
  /// go into code someone already wrote.
  ///
  /// Waits as long as the reader takes, and ends when the page closes.
  ///
  /// # Errors
  ///
  /// The launch or navigation error, or a cancellation where the page
  /// went away before anything was picked.
  pub async fn pick(&self) -> Result<String> {
    let (browser, page) = self.open_page().await?;
    let picked = page.pick_locator().await?;
    let selector = picked.selector().to_string();
    // The reader is done the moment they click, and leaving the window
    // open would make them close it by hand to get their prompt back.
    let _ = browser.close().await;
    Ok(selector)
  }

  /// Run the recorder: launch browser, record, emit code until browser closes or Ctrl+C.
  ///
  /// Code is written as it is recorded, each action once the next one
  /// shows it is final, so the output is useful even mid-session. The file
  /// written at the end is rendered again from the folded recording.
  ///
  /// # Errors
  ///
  /// Returns an error if browser launch or navigation fails.
  pub async fn start(&self) -> Result<()> {
    let emitter = crate::emitter_for(self.options.language, self.options.scaffold);
    let mut stdout = std::io::stdout();
    let live = self.options.output_file.is_none();
    if live {
      let _ = stdout.write_all(emitter.header("").as_bytes());
      let _ = stdout.flush();
    }

    let (browser, page) = self.open_page().await?;
    let source = Arc::new(Mutex::new(crate::emitter::LiveSource::new(Arc::clone(&emitter))));
    let written = Arc::clone(&source);
    let recorder = Recorder::attach(&page, move |event: RecorderEvent| {
      if let RecorderEvent::Action {
        action,
        replaces_previous,
        replaces_index,
      } = event
        && live
        && let Some(code) =
          written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(action, replaces_previous, replaces_index)
      {
        let mut out = std::io::stdout();
        let _ = out.write_all(code.as_bytes());
        let _ = out.flush();
      }
    })
    .await?;

    eprintln!("Recording started. Interact with the browser.");
    eprintln!("Press Ctrl+C or close the browser to stop.\n");

    // Stop on browser close, Ctrl+C (SIGINT), or SIGTERM — finalize the
    // emitted script (footer) in every case so a non-interactive stop
    // (`kill`, editor, CI) still produces a complete, runnable file.
    let closed = wait_for_stop(&page).await?;
    let actions = if closed {
      recorder.actions()
    } else {
      recorder.detach().await
    };

    if live {
      let code = source.lock().unwrap_or_else(PoisonError::into_inner).finish();
      if let Some(code) = code {
        let _ = stdout.write_all(code.as_bytes());
      }
      let _ = stdout.write_all(emitter.footer().as_bytes());
      let _ = stdout.flush();
    } else if let Some(path) = &self.options.output_file {
      let source = crate::emitter::render(
        &*crate::emitter_for(self.options.language, self.options.scaffold),
        &actions,
      );
      std::fs::write(path, source).map_err(|e| format!("cannot write output file {path}: {e}"))?;
    }
    let _ = browser.close().await;
    Ok(())
  }
}

/// Wait for the page to close or the process to be asked to stop. `true`
/// when the page closed.
async fn wait_for_stop(page: &Arc<Page>) -> Result<bool> {
  #[cfg(unix)]
  {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).map_err(|e| FerriError::backend(format!("signal: {e}")))?;
    tokio::select! {
      _ = page.wait_for_event("close", Some(86_400_000)) => Ok(true),
      _ = tokio::signal::ctrl_c() => { eprintln!("\nRecording stopped."); Ok(false) }
      _ = term.recv() => { eprintln!("\nRecording stopped."); Ok(false) }
    }
  }
  #[cfg(not(unix))]
  {
    tokio::select! {
      _ = page.wait_for_event("close", Some(86_400_000)) => Ok(true),
      _ = tokio::signal::ctrl_c() => { eprintln!("\nRecording stopped."); Ok(false) }
    }
  }
}
