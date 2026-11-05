//! Recording a user's interactions on a live browser context.
//!
//! [`Recorder::attach`] works on any page a [`Page`] handle reaches,
//! headless or remote included: the page-side half (`recorder.js`) watches
//! the input the context's pages receive, however it got there (a person at
//! a headed window, CDP `Input.*` events relayed from elsewhere), and
//! reports each action with the best locator the selector generator finds.
//! Every page of the context is recorded, popups included, and every frame
//! of each page. [`HeadedRecorder`] is the `ferridriver codegen` experience
//! built on top: it launches a headed Chromium and writes the recording out
//! as code.

use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use rustc_hash::FxHashMap;
use tokio::sync::{mpsc, oneshot};

use crate::chromium;
use crate::context::ContextRef;
use crate::disposable::Disposable;
use crate::error::{FerriError, Result};
use crate::events::{BindingSource, ContextEvent, ExposedBinding, ListenerId};
use crate::frame::Frame;
use crate::options::{BrowserContextOptions, InitScriptSource, LaunchOptions, ViewportOption};
use crate::page::Page;

use super::{Action, FIRST_PAGE, OutputLanguage, RecordedAction, Scaffold};

const RECORDER_JS: &str = include_str!("recorder.js");
const RECORDER_SUPPORT_JS: &str = include_str!("../injected/dist/recorder-support.min.js");
const BINDING: &str = "__fdRecorderBinding";
const ENTER_FRAME: &str = " >> internal:control=enter-frame >> ";

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
  /// click, a field typed into again, a click found to have opened a
  /// popup), so a consumer keeping a list drops that last entry first.
  Action {
    action: RecordedAction,
    replaces_previous: bool,
  },
  /// The mode changed, including the switch back to recording after an
  /// assertion was picked.
  Mode(RecorderMode),
}

/// Where a [`Recorder`] delivers what it observes: a callback or a channel.
///
/// Called synchronously, in the order the pages performed the actions, so
/// an implementation must not block.
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

/// Contexts that already have a recorder, by composite key. The page-side
/// binding has one name, so two recorders on one context would steal each
/// other's actions.
fn attached_contexts() -> &'static Mutex<rustc_hash::FxHashSet<String>> {
  static CONTEXTS: std::sync::OnceLock<Mutex<rustc_hash::FxHashSet<String>>> = std::sync::OnceLock::new();
  CONTEXTS.get_or_init(Mutex::default)
}

/// A recorded page and the name the code gives it.
struct RecordedPage {
  id: usize,
  alias: String,
  page: Weak<Page>,
}

struct Shared {
  sink: Box<dyn RecorderSink>,
  state: Mutex<State>,
}

#[derive(Default)]
struct State {
  mode: RecorderMode,
  actions: Vec<RecordedAction>,
  pages: Vec<RecordedPage>,
  /// Popups whose opening was already put on the action that opened them.
  announced_popups: rustc_hash::FxHashSet<usize>,
  detached: bool,
}

impl State {
  fn alias_of(&self, id: usize) -> Option<&str> {
    self
      .pages
      .iter()
      .find(|page| page.id == id)
      .map(|page| page.alias.as_str())
  }
}

impl Shared {
  fn state(&self) -> std::sync::MutexGuard<'_, State> {
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// The name of `page` in the generated code, given on first sight: the
  /// page the recording started on is `page`, the rest `page1`, `page2`,
  /// ... in the order they took part. A page another opened is announced
  /// on the opener's last action, which is the one that opened it.
  fn alias_for(&self, page: &Arc<Page>) -> String {
    let id = page.backend_page_id();
    let alias = {
      let mut state = self.state();
      if let Some(alias) = state.alias_of(id) {
        alias.to_string()
      } else {
        let alias = match state.pages.len() {
          0 => FIRST_PAGE.to_string(),
          n => format!("{FIRST_PAGE}{n}"),
        };
        state.pages.push(RecordedPage {
          id,
          alias: alias.clone(),
          page: Arc::downgrade(page),
        });
        alias
      }
    };
    self.announce_popup(page, &alias);
    alias
  }

  fn announce_popup(&self, popup: &Arc<Page>, alias: &str) {
    let Some(opener) = popup.opener() else { return };
    let updated = {
      let mut state = self.state();
      let Some(opener_alias) = state.alias_of(opener.backend_page_id()).map(str::to_string) else {
        return;
      };
      if state.detached || !state.announced_popups.insert(popup.backend_page_id()) {
        return;
      }
      let Some(index) = state.actions.iter().rposition(|action| action.page == opener_alias) else {
        return;
      };
      let last = index + 1 == state.actions.len();
      let opening = &mut state.actions[index];
      opening.popup = Some(alias.to_string());
      // A consumer keeping a list can only replace the last entry; one
      // further back stays as it was streamed and is right in the result.
      last.then(|| opening.clone())
    };
    if let Some(action) = updated {
      self.sink.event(RecorderEvent::Action {
        action,
        replaces_previous: true,
      });
    }
  }

  /// Record `action`, folding it into the previous one where the page
  /// reported the same gesture twice. Returns the mode the page should be
  /// in afterwards.
  fn record(&self, mut action: RecordedAction) -> RecorderMode {
    let (replaces_previous, mode_change, action) = {
      let mut state = self.state();
      if state.detached {
        return RecorderMode::None;
      }
      let replaces_previous = state.actions.last().is_some_and(|last| {
        last.page == action.page
          && matches!(
            (&last.action, &action.action),
            (Action::Click { selector: a, .. }, Action::Dblclick { selector: b, .. })
              | (Action::Fill { selector: a, .. }, Action::Fill { selector: b, .. })
              if a == b
          )
      });
      if replaces_previous && let Some(previous) = state.actions.pop() {
        action.popup = action.popup.or(previous.popup);
      }
      state.actions.push(action.clone());
      let mode_change = (action.action.is_assertion() && state.mode != RecorderMode::Recording).then(|| {
        state.mode = RecorderMode::Recording;
        RecorderMode::Recording
      });
      (replaces_previous, mode_change, action)
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

  /// Handle one message from the page-side recorder, `action` already
  /// carrying selectors that reach into its frame, and answer with the
  /// current mode.
  fn on_message(&self, page: &str, message: &serde_json::Value) -> RecorderMode {
    match message.get("kind").and_then(serde_json::Value::as_str) {
      Some("action") => match message.get("action").cloned().map(serde_json::from_value::<Action>) {
        Some(Ok(action)) => self.record(RecordedAction {
          page: page.to_string(),
          action,
          popup: None,
        }),
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

  /// The page of the most recent action, or the first page.
  fn current_page(&self) -> Option<(String, Arc<Page>)> {
    let state = self.state();
    let alias = state.actions.last().map_or(FIRST_PAGE, |action| action.page.as_str());
    state
      .pages
      .iter()
      .find(|page| page.alias == alias)
      .and_then(|page| Some((page.alias.clone(), page.page.upgrade()?)))
  }
}

/// What reaches the recorder from the browser, in the order it happened.
enum Inbound {
  /// A message the page-side recorder sent, answered with the mode.
  Call {
    source: BindingSource,
    message: serde_json::Value,
    reply: oneshot::Sender<RecorderMode>,
  },
  /// A page opened in the recorded context.
  Page(Arc<Page>),
}

/// Consume what the browser reports, one item at a time so actions keep
/// the order the pages performed them in while a frame's selector path is
/// worked out.
async fn run(shared: Arc<Shared>, mut inbound: mpsc::UnboundedReceiver<Inbound>) {
  let mut frame_paths: FxHashMap<Arc<str>, Vec<String>> = FxHashMap::default();
  while let Some(item) = inbound.recv().await {
    match item {
      Inbound::Page(page) => {
        shared.alias_for(&page);
      },
      Inbound::Call {
        source,
        mut message,
        reply,
      } => {
        let mode = match source.live_page() {
          Some(page) => {
            let alias = shared.alias_for(&page);
            let frame = page.frame_for_id(&source.frame);
            if let Some(action) = message.get_mut("action")
              && frame.parent_frame().is_some()
            {
              if !frame_paths.contains_key(frame.id()) {
                let path = frame_path(&frame).await;
                frame_paths.insert(Arc::clone(frame.id()), path);
              }
              if let Some(path) = frame_paths.get(frame.id()) {
                enter_frames(action, path);
              }
            }
            shared.on_message(&alias, &message)
          },
          // The page went away before its call was handled: nothing to
          // say where the action happened.
          None => shared.state().mode,
        };
        let _ = reply.send(mode);
      },
    }
  }
}

/// Prefix a frame's action with the path into the frame, and say it the
/// way the path reads in code.
fn enter_frames(action: &mut serde_json::Value, path: &[String]) {
  let Some(selector) = action.get("selector").and_then(serde_json::Value::as_str) else {
    return;
  };
  let mut full = path.join(ENTER_FRAME);
  full.push_str(ENTER_FRAME);
  full.push_str(selector);
  action["locator"] = crate::locator_generator::as_locator(&full).into();
  action["selector"] = full.into();
}

/// The selectors of the frame elements from the main frame down to
/// `frame`: each one generated in its parent, or, where that fails, the
/// frame's `name` or `src`.
async fn frame_path(frame: &Frame) -> Vec<String> {
  let mut path = Vec::new();
  let mut current = frame.clone();
  while let Some(parent) = current.parent_frame() {
    let generated = match current.frame_element().await {
      Ok(element) => {
        let selector = element
          .any_element()
          .call_js_fn_value(
            "function() { var fd = window.__fd; \
             return fd && fd._injected ? fd._injected.generateSelectorSimple(this) : null; }",
          )
          .await
          .ok()
          .flatten()
          .and_then(|value| value.as_str().map(str::to_string))
          .filter(|selector| !selector.is_empty());
        let _ = element.dispose().await;
        selector
      },
      Err(_) => None,
    };
    path.push(generated.unwrap_or_else(|| {
      let name = current.name();
      if name.is_empty() {
        format!("iframe[src={}]", quote_css_attribute_value(&current.url()))
      } else {
        format!("iframe[name={}]", quote_css_attribute_value(&name))
      }
    }));
    current = parent;
  }
  path.reverse();
  path
}

fn quote_css_attribute_value(text: &str) -> String {
  let mut quoted = String::with_capacity(text.len() + 2);
  quoted.push('"');
  for ch in text.chars() {
    if ch == '"' || ch == '\\' {
      quoted.push('\\');
    }
    quoted.push(ch);
  }
  quoted.push('"');
  quoted
}

/// A recorder attached to one browser context.
///
/// Records every page of the context until [`Self::detach`]. Dropping it
/// instead stops recording but leaves the page-side scripts in place until
/// the pages navigate away.
pub struct Recorder {
  page: Arc<Page>,
  context: ContextRef,
  shared: Arc<Shared>,
  inbound: mpsc::UnboundedSender<Inbound>,
  worker: tokio::task::JoinHandle<()>,
  init_script: Option<Disposable>,
  binding: Option<Disposable>,
  page_listener: Option<ListenerId>,
  key: String,
}

impl Recorder {
  /// Start recording the context `page` belongs to, delivering every
  /// action to `sink` as a page performs it. `page` is the one the code
  /// calls `page`; pages opened later get names of their own.
  ///
  /// When the page already shows a document, the recording opens with a
  /// navigation to it, so the generated code starts where the user did.
  ///
  /// # Errors
  ///
  /// Fails when the page belongs to no context, when the context already
  /// has a recorder, or when the page-side scripts or binding cannot be
  /// installed.
  pub async fn attach(page: &Arc<Page>, sink: impl RecorderSink) -> Result<Self> {
    let context = page
      .context()
      .cloned()
      .ok_or_else(|| FerriError::invalid_argument("page", "a page outside any browser context cannot be recorded"))?;
    let key = context.composite();
    if !attached_contexts()
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .insert(key.clone())
    {
      return Err(FerriError::invalid_argument(
        "page",
        "a recorder is already attached to this page's context",
      ));
    }
    let shared = Arc::new(Shared {
      sink: Box::new(sink),
      state: Mutex::new(State::default()),
    });
    shared.alias_for(page);
    let (inbound, receiver) = mpsc::unbounded_channel();
    let worker = tokio::spawn(run(Arc::clone(&shared), receiver));
    let mut recorder = Self {
      page: Arc::clone(page),
      context,
      shared,
      inbound,
      worker,
      init_script: None,
      binding: None,
      page_listener: None,
      key,
    };
    if let Err(error) = recorder.install().await {
      recorder.uninstall().await;
      return Err(error);
    }
    let url = page.url();
    if !url.is_empty() && url != "about:blank" {
      recorder
        .shared
        .record(RecordedAction::on_first_page(Action::Navigate { url }));
    }
    Ok(recorder)
  }

  async fn install(&mut self) -> Result<()> {
    let inbound = self.inbound.clone();
    let binding: ExposedBinding = Arc::new(move |source, args| {
      // Queued before the future is returned: the backend runs binding
      // calls in the order the page made them, and that order is the
      // recording.
      let message = args
        .first()
        .and_then(serde_json::Value::as_str)
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .unwrap_or_default();
      let (reply, answer) = oneshot::channel();
      let queued = inbound.send(Inbound::Call { source, message, reply }).is_ok();
      Box::pin(async move {
        let mode = if queued {
          answer.await.unwrap_or(RecorderMode::None)
        } else {
          RecorderMode::None
        };
        Ok(serde_json::Value::from(mode.as_str()).into())
      })
    });
    let pages = self.inbound.clone();
    self.page_listener = Some(self.context.on(
      "page",
      Arc::new(move |event| {
        if let ContextEvent::Page(page) = event {
          let _ = pages.send(Inbound::Page(page));
        }
      }),
    ));
    self.binding = Some(self.context.expose_binding(BINDING, binding).await?);

    let test_id = self.context.test_id_attribute();
    let source = format!(
      "(async () => {{\nawait {};\n{RECORDER_SUPPORT_JS}\n{RECORDER_JS}\n}})()",
      crate::selectors::build_lazy_inject_js_with(&test_id)
    );
    self.init_script = Some(
      self
        .context
        .add_init_script(InitScriptSource::Source(source.clone()), None)
        .await?,
    );
    self.in_every_frame(&source).await;
    Ok(())
  }

  /// Evaluate `expression` in every frame of every page the context has
  /// now. A frame that went away in the meantime has nothing to record, so
  /// failures are only logged.
  async fn in_every_frame(&self, expression: &str) {
    for page in self.context.pages() {
      for frame in page.frames() {
        if let Err(error) = page.inner().evaluate_in_frame(expression, frame.id()).await {
          tracing::debug!(target: "ferridriver::codegen", "recorder: frame {}: {error}", frame.id());
        }
      }
    }
  }

  async fn uninstall(&mut self) {
    if let Some(listener) = self.page_listener.take() {
      self.context.off(listener);
    }
    if let Some(script) = self.init_script.take()
      && let Err(error) = script.dispose().await
    {
      tracing::debug!(target: "ferridriver::codegen", "recorder: removing init script: {error}");
    }
    if let Some(binding) = self.binding.take()
      && let Err(error) = binding.dispose().await
    {
      tracing::debug!(target: "ferridriver::codegen", "recorder: removing binding: {error}");
    }
    attached_contexts()
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .remove(&self.key);
  }

  /// The page the recording started on.
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
  pub fn actions(&self) -> Vec<RecordedAction> {
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

  /// The page an assertion about the page itself is about: the one the
  /// last action was done on.
  fn asserted_page(&self) -> (String, Arc<Page>) {
    self
      .shared
      .current_page()
      .unwrap_or_else(|| (FIRST_PAGE.to_string(), Arc::clone(&self.page)))
  }

  /// Record `toHaveURL` with the current URL of the page last acted on.
  pub async fn assert_url(&self) -> RecordedAction {
    self.flush().await;
    let (alias, page) = self.asserted_page();
    let action = RecordedAction {
      page: alias,
      action: Action::AssertUrl { url: page.url() },
      popup: None,
    };
    self.shared.record(action.clone());
    action
  }

  /// Record `toHaveTitle` with the current title of the page last acted
  /// on.
  ///
  /// # Errors
  ///
  /// Fails when the title cannot be read.
  pub async fn assert_title(&self) -> Result<RecordedAction> {
    self.flush().await;
    let (alias, page) = self.asserted_page();
    let action = RecordedAction {
      page: alias,
      action: Action::AssertTitle {
        title: page.title().await?,
      },
      popup: None,
    };
    self.shared.record(action.clone());
    Ok(action)
  }

  /// Wait until every action the pages reported so far has been recorded.
  ///
  /// The pages report through an exposed binding, whose calls are queued
  /// in order and answered once handled; awaiting one more call is what
  /// proves the earlier ones are in.
  pub async fn flush(&self) {
    self
      .in_every_frame(&format!(
        "(async () => {{ if (window.{BINDING}) await window.{BINDING}(JSON.stringify({{ kind: 'sync' }})); }})()"
      ))
      .await;
  }

  /// Stop recording and remove the page-side scripts. Returns everything
  /// recorded. A page that went away has nothing left to clean, so this
  /// cannot fail.
  pub async fn detach(mut self) -> Vec<RecordedAction> {
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
    self.worker.abort();
    if let Some(listener) = self.page_listener.take() {
      self.context.off(listener);
    }
    attached_contexts()
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .remove(&self.key);
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
  /// Code is written as it is recorded, each action once the next one
  /// shows it is final, so the output is useful even mid-session. The file
  /// written at the end is rendered again from the folded recording.
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
    let source = Arc::new(Mutex::new(super::emitter::LiveSource::new(Arc::clone(&emitter))));
    let written = Arc::clone(&source);
    let recorder = Recorder::attach(&page, move |event: RecorderEvent| {
      if let RecorderEvent::Action {
        action,
        replaces_previous,
      } = event
        && live
        && let Some(code) = written
          .lock()
          .unwrap_or_else(PoisonError::into_inner)
          .push(action, replaces_previous)
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
      if let Some(code) = source.lock().unwrap_or_else(PoisonError::into_inner).finish() {
        let _ = stdout.write_all(code.as_bytes());
      }
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
    shared.on_message(FIRST_PAGE, &message(&click));
    shared.on_message(FIRST_PAGE, &message(&dblclick));
    assert_eq!(
      shared.state().actions,
      [RecordedAction::on_first_page(Action::Dblclick {
        selector: "#b".into(),
        locator: "locator('#b')".into()
      })]
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
    let mode = shared.on_message(
      FIRST_PAGE,
      &message(&serde_json::json!({
        "type": "assertVisible", "selector": "#ok", "locator": "locator('#ok')",
      })),
    );
    assert_eq!(mode, RecorderMode::Recording);
    let seen = seen.lock().unwrap();
    assert_eq!(
      seen.first(),
      Some(&RecorderEvent::Mode(RecorderMode::AssertingVisibility))
    );
    assert_eq!(seen.last(), Some(&RecorderEvent::Mode(RecorderMode::Recording)));
  }

  #[test]
  fn an_action_in_a_frame_reaches_in_through_the_frame_elements() {
    let mut action = serde_json::json!({
      "type": "click",
      "selector": "internal:role=textbox[name=\"Code\"i]",
      "locator": "getByRole('textbox', { name: 'Code' })",
    });
    enter_frames(
      &mut action,
      &["iframe[title=\"Coupon\"]".to_string(), "#inner".to_string()],
    );
    assert_eq!(
      action["selector"],
      "iframe[title=\"Coupon\"] >> internal:control=enter-frame >> #inner >> internal:control=enter-frame >> \
       internal:role=textbox[name=\"Code\"i]"
    );
    assert_eq!(
      action["locator"],
      "locator('iframe[title=\"Coupon\"]').contentFrame().locator('#inner').contentFrame().getByRole('textbox', { name: 'Code' })"
    );
    assert_eq!(quote_css_attribute_value("a\"b\\c"), "\"a\\\"b\\\\c\"");
  }

  #[test]
  fn a_detached_recorder_records_nothing() {
    let (shared, seen) = shared();
    shared.state().detached = true;
    let mode = shared.on_message(
      FIRST_PAGE,
      &message(&serde_json::json!({ "type": "navigate", "url": "https://example.com/" })),
    );
    assert_eq!(mode, RecorderMode::None);
    assert!(seen.lock().unwrap().is_empty());
  }
}
