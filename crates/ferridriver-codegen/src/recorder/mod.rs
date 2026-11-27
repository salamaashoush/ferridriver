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

use std::sync::{Arc, Mutex, PoisonError};

use rustc_hash::FxHashMap;
use tokio::sync::{mpsc, oneshot};

use ferridriver::context::ContextRef;
use ferridriver::disposable::Disposable;
use ferridriver::error::{FerriError, Result};
use ferridriver::events::{BindingSource, ContextEvent, ExposedBinding, ListenerId};
use ferridriver::frame::Frame;
use ferridriver::options::InitScriptSource;
use ferridriver::page::Page;

use super::{Action, FIRST_PAGE, RecordedAction};

const RECORDER_JS: &str = include_str!("../recorder.js");
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

/// What a recording does with a value typed into a secret field: an
/// `<input type=password>`, or a field whose `autocomplete` asks for a
/// password, a one-time code, or a card's number or security code.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Secrets {
  /// The value is recorded as typed.
  #[default]
  Literal,
  /// The value is recorded as the environment variable the code reads in
  /// its place, named after the field. It reaches the sink once, as
  /// [`RecorderEvent::Secret`], and nowhere else.
  Env,
}

/// A secret's name and the value it stands for.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecordedSecret {
  pub name: String,
  pub value: String,
}

/// What the page knows of a secret field, to name its secret by.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
struct SecretField {
  password: bool,
  label: String,
  name: String,
  id: String,
}

impl SecretField {
  /// The field's accessible label as an environment variable name, else
  /// its `name`, else its `id`, else what kind of secret it holds.
  fn base_name(&self) -> String {
    [&self.label, &self.name, &self.id]
      .into_iter()
      .find_map(|text| env_name(text))
      .unwrap_or_else(|| if self.password { "PASSWORD" } else { "SECRET" }.to_string())
  }
}

/// `text` in `UPPER_SNAKE_CASE`: ASCII letters and digits, each run of
/// anything else one `_`, none at the ends, and a `_` before a leading
/// digit. `None` when nothing is left.
fn env_name(text: &str) -> Option<String> {
  let mut name = String::new();
  let mut gap = false;
  for ch in text.chars() {
    if ch.is_ascii_alphanumeric() {
      if gap && !name.is_empty() {
        name.push('_');
      }
      gap = false;
      name.push(ch.to_ascii_uppercase());
    } else {
      gap = true;
    }
  }
  if name.is_empty() {
    return None;
  }
  if name.starts_with(|ch: char| ch.is_ascii_digit()) {
    name.insert(0, '_');
  }
  Some(name)
}

/// Whether `name` can be an environment variable a script reads as
/// `process.env.NAME`.
fn is_env_name(name: &str) -> bool {
  let mut chars = name.chars();
  chars.next().is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
    && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// Something the recorder observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecorderEvent {
  /// An action was recorded. `replaces_index` is the index of the entry it
  /// supersedes in the list of everything recorded: a double click that
  /// began as a click, a field typed into again, an action found to have
  /// opened a popup, however far back. `replaces_previous` is set as well
  /// when that entry is the last one.
  Action {
    action: RecordedAction,
    replaces_previous: bool,
    replaces_index: Option<usize>,
  },
  /// The mode changed, including the switch back to recording after an
  /// assertion was picked.
  Mode(RecorderMode),
  /// The value the secret `name` stands for, under [`Secrets::Env`]. Sent
  /// once the step that typed it is settled (no keystroke can change it),
  /// before the next recorded action, and again only if a later edit of
  /// that same step changed it.
  Secret { name: String, value: String },
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

/// Contexts that already have a recorder. The page-side binding has one
/// name, so two recorders on one context would steal each other's actions.
fn attached_contexts() -> &'static Mutex<rustc_hash::FxHashSet<AttachedContext>> {
  static CONTEXTS: std::sync::OnceLock<Mutex<rustc_hash::FxHashSet<AttachedContext>>> = std::sync::OnceLock::new();
  CONTEXTS.get_or_init(Mutex::default)
}

/// A context as the registry knows it: the composite key names it only
/// within its browser (every browser's first context is `default:default`),
/// so the browser state it lives in comes with it. The recorder holds that
/// state while registered, so its address cannot name another meanwhile.
#[derive(Clone, PartialEq, Eq, Hash)]
struct AttachedContext {
  state: usize,
  key: String,
}

impl AttachedContext {
  fn of(context: &ferridriver::context::ContextRef) -> Self {
    Self {
      state: Arc::as_ptr(context.state()) as usize,
      key: context.composite(),
    }
  }
}

/// A recorded page and the name the code gives it.
struct RecordedPage {
  id: usize,
  alias: String,
  /// Held, not just named: a page reaches a binding call only through a
  /// live handle, and nothing else holds one for a popup a session host
  /// adopted, so without this its actions arrived with no page and were
  /// dropped.
  page: Arc<Page>,
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
  secrets: Secrets,
  /// Every secret name in use and its value, in the order they were given.
  names: Vec<RecordedSecret>,
  /// The value each name was last sent to the sink with.
  announced: FxHashMap<String, String>,
  /// Fields the host made secret, by selector, and the name it chose.
  named_fields: FxHashMap<String, String>,
}

impl State {
  /// Put the popup `alias` on the last action done on `opener`, which is
  /// the one that opened it, and say so as a replacement of that entry.
  fn open_popup(&mut self, opener: &str, alias: &str) -> Option<RecorderEvent> {
    let index = self.actions.iter().rposition(|action| action.page == opener)?;
    let last = index + 1 == self.actions.len();
    let opening = &mut self.actions[index];
    opening.popup = Some(alias.to_string());
    Some(RecorderEvent::Action {
      action: opening.clone(),
      replaces_previous: last,
      replaces_index: Some(index),
    })
  }

  /// Record a fill or value assertion's value as a secret where it is
  /// one: typed into a secret field, into a field the host made secret, or
  /// the value of a secret already named. `previous` is the entry `action`
  /// replaces, whose name lapses if no other step reads it.
  fn hide_secret(&mut self, action: &mut Action, field: Option<&SecretField>, previous: Option<&RecordedAction>) {
    if let Some(lapsed) = previous.and_then(|previous| previous.action.secret())
      && !self.actions.iter().any(|action| action.action.secret() == Some(lapsed))
    {
      self.names.retain(|secret| secret.name != lapsed);
    }
    let (Some(selector), Some(literal)) = (action.selector(), action.typed_value()) else {
      return;
    };
    if literal.is_empty() {
      return;
    }
    if let Some(known) = self.names.iter().find(|secret| secret.value == literal) {
      let name = known.name.clone();
      action.hide_value(&name);
      return;
    }
    let Some(base) = self
      .named_fields
      .get(selector)
      .cloned()
      .or_else(|| field.map(SecretField::base_name))
    else {
      return;
    };
    let name = self.unused_name(&base);
    self.names.push(RecordedSecret {
      name: name.clone(),
      value: literal.to_string(),
    });
    action.hide_value(&name);
  }

  /// `base`, or `base_2`, `base_3`, ... where a different value has it.
  /// One past the number of names in use is always free.
  fn unused_name(&self, base: &str) -> String {
    let taken = |name: &str| self.names.iter().any(|secret| secret.name == name);
    if !taken(base) {
      return base.to_string();
    }
    (2..=self.names.len() + 1)
      .map(|n| format!("{base}_{n}"))
      .find(|name| !taken(name))
      .unwrap_or_else(|| format!("{base}_{}", self.names.len() + 2))
  }

  /// The secrets whose settled value the sink has not been sent, sent now.
  /// Called once no step can change any more: before a step that does not
  /// replace the last one, and when the recording ends.
  fn settle_secrets(&mut self) -> Vec<RecorderEvent> {
    let mut events = Vec::new();
    for secret in &self.names {
      if self.announced.get(&secret.name) == Some(&secret.value) {
        continue;
      }
      self.announced.insert(secret.name.clone(), secret.value.clone());
      events.push(RecorderEvent::Secret {
        name: secret.name.clone(),
        value: secret.value.clone(),
      });
    }
    events
  }

  /// The secrets the recorded steps read, in the order they were named.
  fn secrets_in_use(&self) -> Vec<RecordedSecret> {
    self
      .names
      .iter()
      .filter(|secret| {
        self
          .actions
          .iter()
          .any(|action| action.action.secret() == Some(secret.name.as_str()))
      })
      .cloned()
      .collect()
  }

  /// Make step `index` the secret `name`, with every later step holding
  /// the same value, and have later values typed into its field use the
  /// name too. Answers the indices rewritten and the events that say so.
  fn make_secret(&mut self, index: usize, name: &str) -> Result<(Vec<usize>, Vec<RecorderEvent>)> {
    if self.secrets != Secrets::Env {
      return Err(FerriError::invalid_argument(
        "secrets",
        "only a recording started with secrets \"env\" keeps secrets",
      ));
    }
    if !is_env_name(name) {
      return Err(FerriError::invalid_argument(
        "name",
        format!("'{name}' is not an environment variable name: use letters, digits and _, not starting with a digit"),
      ));
    }
    let step = self.actions.get(index).ok_or_else(|| {
      FerriError::invalid_argument("step", format!("no step {index}: {} recorded", self.actions.len()))
    })?;
    if let Some(existing) = step.action.secret() {
      return if existing == name {
        Ok((Vec::new(), Vec::new()))
      } else {
        Err(FerriError::invalid_argument(
          "step",
          format!("step {index} is already the secret {existing}"),
        ))
      };
    }
    let (Some(selector), Some(literal)) = (step.action.selector(), step.action.typed_value()) else {
      return Err(FerriError::invalid_argument(
        "step",
        format!("step {index} types no value to keep secret"),
      ));
    };
    let (selector, literal) = (selector.to_string(), literal.to_string());
    if literal.is_empty() {
      return Err(FerriError::invalid_argument(
        "step",
        format!("step {index} types nothing"),
      ));
    }
    if self
      .names
      .iter()
      .any(|secret| secret.name == name && secret.value != literal)
    {
      return Err(FerriError::invalid_argument(
        "name",
        format!("{name} already stands for another value"),
      ));
    }
    if !self.names.iter().any(|secret| secret.name == name) {
      self.names.push(RecordedSecret {
        name: name.to_string(),
        value: literal.clone(),
      });
    }
    self.named_fields.insert(selector, name.to_string());
    self.announced.insert(name.to_string(), literal.clone());
    let mut events = vec![RecorderEvent::Secret {
      name: name.to_string(),
      value: literal.clone(),
    }];
    let last = self.actions.len().saturating_sub(1);
    let mut rewritten = Vec::new();
    for (at, step) in self.actions.iter_mut().enumerate().skip(index) {
      if step.action.typed_value() == Some(literal.as_str()) {
        step.action.hide_value(name);
        rewritten.push(at);
        events.push(RecorderEvent::Action {
          action: step.clone(),
          replaces_previous: at == last,
          replaces_index: Some(at),
        });
      }
    }
    Ok((rewritten, events))
  }

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
          page: Arc::clone(page),
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
      let Some(updated) = state.open_popup(&opener_alias, alias) else {
        return;
      };
      updated
    };
    self.sink.event(updated);
  }

  /// Record `action`, folding it into the previous one where the page
  /// reported the same gesture twice. Returns the mode the page should be
  /// in afterwards.
  fn record(&self, action: RecordedAction) -> RecorderMode {
    self.record_typed(action, None)
  }

  /// [`Self::record`] for an action the page says was typed into the
  /// secret `field`.
  fn record_typed(&self, mut action: RecordedAction, field: Option<&SecretField>) -> RecorderMode {
    let (settled, replaces_index, mode_change, action) = {
      let mut state = self.state();
      if state.detached {
        return RecorderMode::None;
      }
      let replaces_previous = state.actions.last().is_some_and(|last| {
        last.page == action.page
          && (action.action.continues_click(&last.action)
            || matches!(
              (&last.action, &action.action),
              (Action::Fill { selector: a, .. }, Action::Fill { selector: b, .. }) if a == b
            ))
      });
      let previous = if replaces_previous { state.actions.pop() } else { None };
      if let Some(previous) = &previous {
        action.popup = action.popup.or_else(|| previous.popup.clone());
      }
      let settled = if replaces_previous {
        Vec::new()
      } else {
        state.settle_secrets()
      };
      if state.secrets == Secrets::Env {
        state.hide_secret(&mut action.action, field, previous.as_ref());
      }
      let index = state.actions.len();
      state.actions.push(action.clone());
      let mode_change = (action.action.is_assertion() && state.mode != RecorderMode::Recording).then(|| {
        state.mode = RecorderMode::Recording;
        RecorderMode::Recording
      });
      (settled, replaces_previous.then_some(index), mode_change, action)
    };
    for secret in settled {
      self.sink.event(secret);
    }
    self.sink.event(RecorderEvent::Action {
      action,
      replaces_previous: replaces_index.is_some(),
      replaces_index,
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
        Some(Ok(action)) => {
          let field = message
            .get("secretField")
            .cloned()
            .and_then(|field| serde_json::from_value::<SecretField>(field).ok());
          self.record_typed(
            RecordedAction {
              page: page.to_string(),
              action,
              popup: None,
            },
            field.as_ref(),
          )
        },
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
      .map(|page| (page.alias.clone(), Arc::clone(&page.page)))
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
                enter_frames(action, &page.main_frame(), &frame, path).await;
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
///
/// A path two or more frames deep is shortened where it can be: the
/// selector starts in any frame instead, with as few of the innermost
/// frame elements in front of it as still pick out this frame alone.
async fn enter_frames(action: &mut serde_json::Value, main: &Frame, frame: &Frame, path: &[String]) {
  let Some(selector) = action.get("selector").and_then(serde_json::Value::as_str) else {
    return;
  };
  let mut full = through_frames(path, selector);
  let shortest = async {
    for candidate in any_frame_candidates(path, selector) {
      if ferridriver::locator::any_frame_resolves_to(main, &candidate, frame).await {
        return Some(candidate);
      }
    }
    None
  };
  if let Ok(Some(shorter)) = tokio::time::timeout(FRAME_SEARCH, shortest).await {
    full = shorter;
  }
  action["locator"] = ferridriver::locator_generator::as_locator(&full).into();
  action["selector"] = full.into();
}

/// How long the search for a shorter frame path may take before the full
/// path is used.
const FRAME_SEARCH: std::time::Duration = std::time::Duration::from_secs(2);

fn through_frames(path: &[String], selector: &str) -> String {
  let mut full = path.join(ENTER_FRAME);
  if !full.is_empty() {
    full.push_str(ENTER_FRAME);
  }
  full.push_str(selector);
  full
}

/// The any-frame forms of a path, shortest first, down to the one that
/// still names two frame elements fewer than the full path.
fn any_frame_candidates(path: &[String], selector: &str) -> Vec<String> {
  if path.len() < 2 {
    return Vec::new();
  }
  (2..=path.len())
    .rev()
    .map(|skipped| {
      format!(
        "{} >> {}",
        ferridriver::locator::ANY_FRAME,
        through_frames(&path[skipped..], selector)
      )
    })
    .collect()
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

/// Everything a recording recorded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recorded {
  pub actions: Vec<RecordedAction>,
  /// Under [`Secrets::Env`], the secrets the actions read.
  pub secrets: Vec<RecordedSecret>,
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
  key: AttachedContext,
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
    Self::attach_with(page, Secrets::Literal, sink).await
  }

  /// [`Self::attach`], doing `secrets` with the values typed into secret
  /// fields.
  ///
  /// # Errors
  ///
  /// As [`Self::attach`].
  pub async fn attach_with(page: &Arc<Page>, secrets: Secrets, sink: impl RecorderSink) -> Result<Self> {
    let context = page
      .context()
      .cloned()
      .ok_or_else(|| FerriError::invalid_argument("page", "a page outside any browser context cannot be recorded"))?;
    let key = AttachedContext::of(&context);
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
      state: Mutex::new(State {
        secrets,
        ..State::default()
      }),
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
    let recorder_support = ferridriver::selectors::RECORDER_SUPPORT_JS;
    let source = format!(
      "(async () => {{\nawait {};\n{recorder_support}\n{RECORDER_JS}\n}})()",
      ferridriver::selectors::build_lazy_inject_js_with(&test_id)
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
  pub async fn detach(self) -> Vec<RecordedAction> {
    self.finish().await.actions
  }

  /// [`Self::detach`], answering the secrets the steps read as well, once
  /// the sink has been sent each one's settled value.
  pub async fn finish(mut self) -> Recorded {
    self.flush().await;
    let (settled, recorded) = {
      let mut state = self.shared.state();
      let settled = state.settle_secrets();
      state.detached = true;
      (
        settled,
        Recorded {
          actions: state.actions.clone(),
          secrets: state.secrets_in_use(),
        },
      )
    };
    for secret in settled {
      self.shared.sink.event(secret);
    }
    self
      .in_every_frame("window.__fdRecorderTeardown && window.__fdRecorderTeardown()")
      .await;
    self.uninstall().await;
    recorded
  }

  /// Make step `index`, a fill or value assertion, the secret `name`, as
  /// the detection would have had the field been a secret one: it and
  /// every later step typing the same value are rewritten, the sink is
  /// sent the value and the rewritten steps, and later values typed into
  /// that field are secrets too. Answers the indices rewritten.
  ///
  /// # Errors
  ///
  /// Refused unless the recording keeps secrets ([`Secrets::Env`]), for a
  /// name no environment variable can have, a step that types no value or
  /// is another secret already, and a name that stands for another value.
  pub async fn secret(&self, index: usize, name: &str) -> Result<Vec<usize>> {
    self.flush().await;
    let (rewritten, events) = self.shared.state().make_secret(index, name)?;
    for event in events {
      self.shared.sink.event(event);
    }
    Ok(rewritten)
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

mod headed;
#[cfg(test)]
mod tests;

pub use headed::{HeadedRecorder, RecorderOptions};
