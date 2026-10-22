//! Recording a user's interactions on a live page.
//!
//! [`Recorder::attach`] works on any page a [`Page`] handle reaches,
//! headless or remote included: the page-side half (`recorder.js`) watches
//! the trusted input the page receives, however it got there (a person at a
//! headed window, CDP `Input.*` events relayed from elsewhere), and reports
//! each action with the best locator the selector generator finds.
//! [`HeadedRecorder`] is the `ferridriver codegen` experience built on top:
//! it launches a headed Chromium and writes the recording out as code.

use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};

use crate::chromium;
use crate::disposable::Disposable;
use crate::error::{FerriError, Result};
use crate::events::ExposedFn;
use crate::options::{BrowserContextOptions, InitScriptSource, LaunchOptions, ViewportOption};
use crate::page::Page;

use super::{Action, OutputLanguage, Scaffold};

const RECORDER_JS: &str = include_str!("recorder.js");
const RECORDER_SUPPORT_JS: &str = include_str!("../injected/dist/recorder-support.min.js");
const BINDING: &str = "__fdRecorderBinding";

/// What the next click on the page means. The names are Playwright's
/// recorder modes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecorderMode {
  /// Interactions are recorded as actions.
  #[default]
  Recording,
  /// Nothing is recorded; the page behaves as if no recorder were attached.
  None,
  /// The next click records `toBeVisible()` on the element instead of
  /// clicking it.
  AssertingVisibility,
  /// The next click records `toContainText()` with the element's text.
  AssertingText,
  /// The next click records `toHaveValue()` (or `toBeChecked()`) on a form
  /// control.
  AssertingValue,
}

impl RecorderMode {
  /// The wire name, as the page-side recorder and the session protocol spell it.
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Recording => "recording",
      Self::None => "none",
      Self::AssertingVisibility => "assertingVisibility",
      Self::AssertingText => "assertingText",
      Self::AssertingValue => "assertingValue",
    }
  }

  /// Parse a wire name.
  ///
  /// # Errors
  ///
  /// Names the accepted modes when `s` is none of them.
  pub fn parse(s: &str) -> std::result::Result<Self, String> {
    [
      Self::Recording,
      Self::None,
      Self::AssertingVisibility,
      Self::AssertingText,
      Self::AssertingValue,
    ]
    .into_iter()
    .find(|mode| mode.as_str() == s)
    .ok_or_else(|| {
      format!(
        "unknown recorder mode '{s}': expected recording, none, assertingVisibility, assertingText or assertingValue"
      )
    })
  }
}

/// Something the recorder observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecorderEvent {
  /// An action was recorded. `replaces_previous` is set when it supersedes
  /// the action recorded just before it (a double click that began as a
  /// click, a field typed into again before anything else happened), so a
  /// consumer keeping a list drops that last entry first.
  Action { action: Action, replaces_previous: bool },
  /// The mode changed, including the switch back to recording after an
  /// assertion was picked.
  Mode(RecorderMode),
}

/// Where a [`Recorder`] delivers what it observes: a callback or a channel.
///
/// Called synchronously, in the order the page performed the actions, so an
/// implementation must not block.
pub trait RecorderSink: Send + Sync + 'static {
  fn event(&self, event: RecorderEvent);
}

impl<F> RecorderSink for F
where
  F: Fn(RecorderEvent) + Send + Sync + 'static,
{
  fn event(&self, event: RecorderEvent) {
    self(event);
  }
}

impl RecorderSink for tokio::sync::mpsc::UnboundedSender<RecorderEvent> {
  fn event(&self, event: RecorderEvent) {
    let _ = self.send(event);
  }
}

/// Pages that already have a recorder, by [`Page::backend_page_id`]. The
/// page-side binding has one name, so two recorders on one page would steal
/// each other's actions.
fn attached_pages() -> &'static Mutex<rustc_hash::FxHashSet<usize>> {
  static PAGES: std::sync::OnceLock<Mutex<rustc_hash::FxHashSet<usize>>> = std::sync::OnceLock::new();
  PAGES.get_or_init(Mutex::default)
}

struct Shared {
  sink: Box<dyn RecorderSink>,
  state: Mutex<State>,
}

#[derive(Default)]
struct State {
  mode: RecorderMode,
  actions: Vec<Action>,
  detached: bool,
}

impl Shared {
  fn state(&self) -> std::sync::MutexGuard<'_, State> {
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Record `action`, folding it into the previous one where the page
  /// reported the same gesture twice. Returns the mode the page should be
  /// in afterwards.
  fn record(&self, action: Action) -> RecorderMode {
    let (replaces_previous, mode_change) = {
      let mut state = self.state();
      if state.detached {
        return RecorderMode::None;
      }
      let replaces_previous = matches!(
        (state.actions.last(), &action),
        (Some(Action::Click { selector: a, .. }), Action::Dblclick { selector: b, .. })
          | (Some(Action::Fill { selector: a, .. }), Action::Fill { selector: b, .. })
          if a == b
      );
      if replaces_previous {
        state.actions.pop();
      }
      state.actions.push(action.clone());
      let mode_change = (action.is_assertion() && state.mode != RecorderMode::Recording).then(|| {
        state.mode = RecorderMode::Recording;
        RecorderMode::Recording
      });
      (replaces_previous, mode_change)
    };
    self.sink.event(RecorderEvent::Action {
      action,
      replaces_previous,
    });
    if let Some(mode) = mode_change {
      self.sink.event(RecorderEvent::Mode(mode));
    }
    self.state().mode
  }

  fn set_mode(&self, mode: RecorderMode) {
    let changed = {
      let mut state = self.state();
      let changed = state.mode != mode;
      state.mode = mode;
      changed
    };
    if changed {
      self.sink.event(RecorderEvent::Mode(mode));
    }
  }

  /// Handle one message from the page-side recorder and answer with the
  /// current mode.
  fn on_message(&self, message: &serde_json::Value) -> RecorderMode {
    match message.get("kind").and_then(serde_json::Value::as_str) {
      Some("action") => match message.get("action").cloned().map(serde_json::from_value::<Action>) {
        Some(Ok(action)) => self.record(action),
        Some(Err(error)) => {
          tracing::debug!(target: "ferridriver::codegen", "unreadable recorded action: {error}");
          self.state().mode
        },
        None => self.state().mode,
      },
      Some("mode") => {
        if let Some(Ok(mode)) = message
          .get("mode")
          .and_then(serde_json::Value::as_str)
          .map(RecorderMode::parse)
        {
          self.set_mode(mode);
        }
        self.state().mode
      },
      _ => self.state().mode,
    }
  }
}

/// A recorder attached to one page.
///
/// Records until [`Self::detach`]. Dropping it instead stops recording but
/// leaves the page-side scripts in place until the page navigates away.
pub struct Recorder {
  page: Arc<Page>,
  shared: Arc<Shared>,
  init_script: Option<Disposable>,
  page_id: usize,
}

impl Recorder {
  /// Start recording on `page`, delivering every action to `sink` as the
  /// page performs it.
  ///
  /// When the page already shows a document, the recording opens with a
  /// navigation to it, so the generated code starts where the user did.
  ///
  /// # Errors
  ///
  /// Fails when the page already has a recorder, or when the page-side
  /// scripts or binding cannot be installed.
  pub async fn attach(page: &Arc<Page>, sink: impl RecorderSink) -> Result<Self> {
    let page_id = page.backend_page_id();
    if !attached_pages()
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .insert(page_id)
    {
      return Err(FerriError::invalid_argument(
        "page",
        "a recorder is already attached to this page",
      ));
    }
    let shared = Arc::new(Shared {
      sink: Box::new(sink),
      state: Mutex::new(State::default()),
    });
    let mut recorder = Self {
      page: Arc::clone(page),
      shared,
      init_script: None,
      page_id,
    };
    if let Err(error) = recorder.install().await {
      recorder.uninstall().await;
      return Err(error);
    }
    let url = page.url();
    if !url.is_empty() && url != "about:blank" {
      recorder.shared.record(Action::Navigate { url });
    }
    Ok(recorder)
  }

  async fn install(&mut self) -> Result<()> {
    let shared = Arc::downgrade(&self.shared);
    let binding: ExposedFn = Arc::new(move |args: Vec<serde_json::Value>| {
      // Handled before the future is returned: the backend runs binding
      // calls in the order the page made them, and that order is the
      // recording.
      let mode = match shared.upgrade() {
        Some(shared) => {
          let message = args
            .first()
            .and_then(serde_json::Value::as_str)
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .unwrap_or_default();
          shared.on_message(&message)
        },
        None => RecorderMode::None,
      };
      Box::pin(std::future::ready(Ok(serde_json::Value::from(mode.as_str()).into())))
    });
    self.page.expose_function(BINDING, binding).await?;

    let test_id = self.page.context().map_or_else(
      crate::selectors::default_test_id_attribute,
      crate::context::ContextRef::test_id_attribute,
    );
    let source = format!(
      "(async () => {{\nawait {};\n{RECORDER_SUPPORT_JS}\n{RECORDER_JS}\n}})()",
      crate::selectors::build_lazy_inject_js_with(&test_id)
    );
    self.init_script = Some(
      self
        .page
        .add_init_script(InitScriptSource::Source(source.clone()), None)
        .await?,
    );
    self.in_every_frame(&source).await;
    Ok(())
  }

  /// Evaluate `expression` in every frame the page has now. A frame that
  /// went away in the meantime has nothing to record, so failures are
  /// only logged.
  async fn in_every_frame(&self, expression: &str) {
    for frame in self.page.frames() {
      if let Err(error) = self.page.inner().evaluate_in_frame(expression, frame.id()).await {
        tracing::debug!(target: "ferridriver::codegen", "recorder: frame {}: {error}", frame.id());
      }
    }
  }

  async fn uninstall(&mut self) {
    if let Some(script) = self.init_script.take()
      && let Err(error) = script.dispose().await
    {
      tracing::debug!(target: "ferridriver::codegen", "recorder: removing init script: {error}");
    }
    if let Err(error) = self.page.remove_exposed_function(BINDING).await {
      tracing::debug!(target: "ferridriver::codegen", "recorder: removing binding: {error}");
    }
    attached_pages()
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .remove(&self.page_id);
  }

  /// The page being recorded.
  #[must_use]
  pub fn page(&self) -> &Arc<Page> {
    &self.page
  }

  /// The current mode.
  #[must_use]
  pub fn mode(&self) -> RecorderMode {
    self.shared.state().mode
  }

  /// Everything recorded so far, after folding.
  #[must_use]
  pub fn actions(&self) -> Vec<Action> {
    self.shared.state().actions.clone()
  }

  /// Change what the next click means. An assertion mode lasts for one
  /// pick, then the recorder returns to [`RecorderMode::Recording`] on its
  /// own and reports the change to the sink.
  pub async fn set_mode(&self, mode: RecorderMode) {
    self.shared.set_mode(mode);
    self
      .in_every_frame(&format!(
        "window.__fdRecorderSetMode && window.__fdRecorderSetMode({})",
        serde_json::Value::from(mode.as_str())
      ))
      .await;
  }

  /// Record `toHaveURL` with the page's current URL.
  pub async fn assert_url(&self) -> Action {
    self.flush().await;
    let action = Action::AssertUrl { url: self.page.url() };
    self.shared.record(action.clone());
    action
  }

  /// Record `toHaveTitle` with the page's current title.
  ///
  /// # Errors
  ///
  /// Fails when the title cannot be read.
  pub async fn assert_title(&self) -> Result<Action> {
    self.flush().await;
    let action = Action::AssertTitle {
      title: self.page.title().await?,
    };
    self.shared.record(action.clone());
    Ok(action)
  }

  /// Send what the page has buffered (a field still being typed into), and
  /// wait until every action the page reported so far has been recorded.
  ///
  /// The page reports through an exposed binding, whose calls the backend
  /// processes in order; awaiting one more call after the flush is what
  /// proves the earlier ones are in.
  pub async fn flush(&self) {
    self
      .in_every_frame(&format!(
        "(async () => {{ if (window.__fdRecorderFlush) window.__fdRecorderFlush(); \
         if (window.{BINDING}) await window.{BINDING}(JSON.stringify({{ kind: 'sync' }})); }})()"
      ))
      .await;
  }

  /// Stop recording and remove the page-side scripts. Returns everything
  /// recorded. A page that went away has nothing left to clean, so this
  /// cannot fail.
  pub async fn detach(mut self) -> Vec<Action> {
    self.flush().await;
    let actions = {
      let mut state = self.shared.state();
      state.detached = true;
      state.actions.clone()
    };
    self
      .in_every_frame("window.__fdRecorderTeardown && window.__fdRecorderTeardown()")
      .await;
    self.uninstall().await;
    actions
  }
}

impl Drop for Recorder {
  fn drop(&mut self) {
    self.shared.state().detached = true;
    attached_pages()
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .remove(&self.page_id);
  }
}

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

  async fn open_page(&self) -> Result<(crate::Browser, Arc<Page>)> {
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
    let ctx = browser.new_context_impl(ctx_opts, true);
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
  /// Code is written as it is recorded, so the output is useful even
  /// mid-session; a double click or a field typed into twice therefore
  /// appears as the actions that were seen. The file written at the end is
  /// rendered again from the folded recording.
  ///
  /// # Errors
  ///
  /// Returns an error if browser launch or navigation fails.
  pub async fn start(&self) -> Result<()> {
    let emitter = self.options.language.emitter_for(self.options.scaffold);
    let mut stdout = std::io::stdout();
    let live = self.options.output_file.is_none();
    if live {
      let _ = stdout.write_all(emitter.header("").as_bytes());
      let _ = stdout.flush();
    }

    let (browser, page) = self.open_page().await?;
    let line_emitter = Arc::clone(&emitter);
    let recorder = Recorder::attach(&page, move |event: RecorderEvent| {
      if let RecorderEvent::Action { action, .. } = event
        && live
      {
        let mut out = std::io::stdout();
        let _ = out.write_all(line_emitter.action(&action).as_bytes());
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
      let _ = stdout.write_all(emitter.footer().as_bytes());
      let _ = stdout.flush();
    } else if let Some(path) = &self.options.output_file {
      let source = super::emitter::render(&*self.options.language.emitter_for(self.options.scaffold), &actions);
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

#[cfg(test)]
mod tests {
  use super::*;

  fn shared() -> (Arc<Shared>, Arc<Mutex<Vec<RecorderEvent>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink_seen = Arc::clone(&seen);
    let shared = Arc::new(Shared {
      sink: Box::new(move |event| sink_seen.lock().unwrap().push(event)),
      state: Mutex::new(State::default()),
    });
    (shared, seen)
  }

  fn message(action: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "kind": "action", "action": action })
  }

  #[test]
  fn modes_round_trip_through_their_wire_names() {
    for name in [
      "recording",
      "none",
      "assertingVisibility",
      "assertingText",
      "assertingValue",
    ] {
      let mode = RecorderMode::parse(name).unwrap();
      assert_eq!(mode.as_str(), name);
      assert_eq!(serde_json::to_value(mode).unwrap(), name);
    }
    assert!(RecorderMode::parse("inspecting").unwrap_err().contains("assertingText"));
  }

  #[test]
  fn a_double_click_replaces_the_click_it_began_as() {
    let (shared, seen) = shared();
    let button = serde_json::json!({ "selector": "#b", "locator": "locator('#b')" });
    let mut click = button.clone();
    click["type"] = "click".into();
    let mut dblclick = button;
    dblclick["type"] = "dblclick".into();
    shared.on_message(&message(&click));
    shared.on_message(&message(&dblclick));
    assert_eq!(
      shared.state().actions,
      [Action::Dblclick {
        selector: "#b".into(),
        locator: "locator('#b')".into()
      }]
    );
    let flags: Vec<bool> = seen
      .lock()
      .unwrap()
      .iter()
      .map(|event| {
        matches!(
          event,
          RecorderEvent::Action {
            replaces_previous: true,
            ..
          }
        )
      })
      .collect();
    assert_eq!(flags, [false, true]);
  }

  #[test]
  fn an_assertion_returns_the_recorder_to_recording() {
    let (shared, seen) = shared();
    shared.set_mode(RecorderMode::AssertingVisibility);
    let mode = shared.on_message(&message(&serde_json::json!({
      "type": "assertVisible", "selector": "#ok", "locator": "locator('#ok')",
    })));
    assert_eq!(mode, RecorderMode::Recording);
    let seen = seen.lock().unwrap();
    assert_eq!(
      seen.first(),
      Some(&RecorderEvent::Mode(RecorderMode::AssertingVisibility))
    );
    assert_eq!(seen.last(), Some(&RecorderEvent::Mode(RecorderMode::Recording)));
  }

  #[test]
  fn a_detached_recorder_records_nothing() {
    let (shared, seen) = shared();
    shared.state().detached = true;
    let mode = shared.on_message(&message(
      &serde_json::json!({ "type": "navigate", "url": "https://example.com/" }),
    ));
    assert_eq!(mode, RecorderMode::None);
    assert!(seen.lock().unwrap().is_empty());
  }
}
