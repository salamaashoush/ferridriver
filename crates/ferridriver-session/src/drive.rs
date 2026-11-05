//! Watching and driving a bound browser from somewhere else: screencast
//! frames out, input events in, and a recorder whose actions stream back.
//!
//! These are verbs of their own rather than scripts because each is either a
//! stream (`screencast`, `recorder.start`) or something sent many times a
//! second (`input`), where compiling a script per event would cost more than
//! the event.

use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use ferridriver::codegen::emitter::{CodeEmitter, TypeScriptTestEmitter};
use ferridriver::codegen::recorder::{Recorder, RecorderEvent, RecorderMode, Secrets};
use ferridriver::codegen::{OutputLanguage, Scaffold};
use ferridriver::options::MouseButton;
use ferridriver::page::{MouseDownOptions, MouseUpOptions};
use ferridriver::{Browser, Page};
use rustc_hash::FxHashMap;
use tokio::sync::watch;

use crate::browser_dispatch::{context_key_for, page_for, page_with_target_id};
use crate::dispatch::EventSink;
use crate::protocol::{
  EventPayload, InputEvent, InputRequest, PageAssertion, RecorderAssertRequest, RecorderModeRequest,
  RecorderSecretRequest, RecorderStartRequest, RecordingResult, ScreencastRequest,
};

type Outcome<T> = std::result::Result<T, String>;

fn parse<T: serde::de::DeserializeOwned>(verb: &str, args: serde_json::Value) -> Outcome<T> {
  serde_json::from_value(args).map_err(|e| format!("malformed {verb} request: {e}"))
}

async fn page(browser: &Browser, context: &str) -> Outcome<Arc<Page>> {
  page_for(browser, context)
    .await
    .map_err(|e| format!("opening a page for context '{context}': {e}"))
}

// ── input ──

fn button(name: Option<&str>) -> Outcome<MouseButton> {
  match name {
    None => Ok(MouseButton::Left),
    Some(name) => MouseButton::parse(name).ok_or_else(|| format!("unknown mouse button '{name}'")),
  }
}

/// Dispatch one input event through the page's own input devices, which
/// speak the backend's input protocol directly (CDP `Input.*` on Chromium):
/// to the page the request names, or else to the context's active page.
pub(crate) async fn input(browser: &Browser, context: &str, args: serde_json::Value) -> Outcome<()> {
  let InputRequest { event, page: target } = parse("input", args)?;
  let page = match target {
    Some(target) => page_with_target_id(browser, context, &target)
      .await
      .map_err(|e| e.to_string())?,
    None => page(browser, context).await?,
  };
  let mouse = page.mouse();
  let keyboard = page.keyboard();
  let result = match event {
    InputEvent::MouseMove { x, y } => mouse.r#move(x, y).await,
    InputEvent::MouseDown {
      x,
      y,
      button: name,
      click_count,
    } => {
      let button = button(name.as_deref())?;
      // Each command reaches a fresh page handle, so the pointer is put
      // where the press happens rather than trusted to be there.
      mouse.r#move(x, y).await.map_err(|e| e.to_string())?;
      mouse
        .down()
        .options(MouseDownOptions {
          button: Some(button),
          click_count: Some(click_count),
        })
        .await
    },
    InputEvent::MouseUp {
      x,
      y,
      button: name,
      click_count,
    } => {
      let button = button(name.as_deref())?;
      mouse.r#move(x, y).await.map_err(|e| e.to_string())?;
      mouse
        .up()
        .options(MouseUpOptions {
          button: Some(button),
          click_count: Some(click_count),
        })
        .await
    },
    InputEvent::MouseWheel { x, y, delta_x, delta_y } => {
      mouse.r#move(x, y).await.map_err(|e| e.to_string())?;
      mouse.wheel(delta_x, delta_y).await
    },
    // A viewer on another layout sends characters the US keyboard has no
    // key for; they arrive as the text they type.
    InputEvent::KeyDown { key } if typed_without_a_key(&key) => keyboard.insert_text(&key).await,
    InputEvent::KeyUp { key } if typed_without_a_key(&key) => Ok(()),
    InputEvent::KeyDown { key } => keyboard.down(&key).await,
    InputEvent::KeyUp { key } => keyboard.up(&key).await,
    InputEvent::InsertText { text } => keyboard.insert_text(&text).await,
  };
  result.map_err(|e| e.to_string())
}

fn typed_without_a_key(key: &str) -> bool {
  key.chars().count() == 1 && ferridriver_types::keyboard::describe(key, false).is_none()
}

// ── screencast ──

/// The CSS-pixel viewport, which the frame events report so a viewer can
/// map a click on a scaled image back to page coordinates.
async fn viewport(page: &Page) -> (u32, u32) {
  let emulated = page
    .viewport_size()
    .and_then(|(w, h)| Some((u32::try_from(w).ok()?, u32::try_from(h).ok()?)));
  if let Some(size) = emulated {
    return size;
  }
  let measured = page
    .inner()
    .evaluate("JSON.stringify([window.innerWidth, window.innerHeight])")
    .await
    .ok()
    .flatten()
    .and_then(|value| {
      value
        .as_str()
        .and_then(|raw| serde_json::from_str::<(u32, u32)>(raw).ok())
    });
  measured.unwrap_or((1280, 720))
}

fn frame_millis(seconds: f64) -> u64 {
  std::time::Duration::try_from_secs_f64(seconds)
    .map_or(0, |clock| u64::try_from(clock.as_millis()).unwrap_or(u64::MAX))
}

/// Stream frames to `events` until the client hangs up or the page closes.
pub(crate) async fn screencast(
  browser: &Browser,
  context: &str,
  args: serde_json::Value,
  events: &EventSink,
) -> Outcome<()> {
  let request: ScreencastRequest = parse("screencast", args)?;
  let page = page(browser, context).await?;
  let (viewport_width, viewport_height) = viewport(&page).await;
  let mut frames = page
    .screencast_frames(
      request.quality.unwrap_or(80),
      request.max_width.unwrap_or(viewport_width),
      request.max_height.unwrap_or(viewport_height),
    )
    .await
    .map_err(|e| format!("starting the screencast: {e}"))?;
  loop {
    tokio::select! {
      () = events.closed() => break,
      frame = frames.next() => {
        let Some((data, timestamp)) = frame else { break };
        let (viewport_width, viewport_height) = page
          .viewport_size()
          .and_then(|(w, h)| Some((u32::try_from(w).ok()?, u32::try_from(h).ok()?)))
          .unwrap_or((viewport_width, viewport_height));
        events.send(EventPayload::Frame {
          data: base64::engine::general_purpose::STANDARD.encode(data),
          timestamp_ms: frame_millis(timestamp),
          viewport_width,
          viewport_height,
        });
      },
    }
  }
  frames.stop().await;
  Ok(())
}

// ── recorder ──

/// The recordings running on one bound browser, by context.
#[derive(Default)]
pub(crate) struct Recordings {
  by_context: Mutex<FxHashMap<String, Arc<Recording>>>,
}

pub(crate) struct Recording {
  recorder: tokio::sync::Mutex<Option<Recorder>>,
  stop: tokio::sync::Notify,
  result: watch::Sender<Option<Arc<RecordingResult>>>,
  language: OutputLanguage,
  scaffold: Scaffold,
  title: Option<String>,
}

impl Recording {
  fn emitter(&self) -> Arc<dyn CodeEmitter> {
    match (&self.title, self.language, self.scaffold) {
      (Some(title), OutputLanguage::TypeScript, Scaffold::Test) => Arc::new(TypeScriptTestEmitter::new(title.clone())),
      _ => self.language.emitter_for(self.scaffold),
    }
  }
}

/// Removes a recording from the table when its `recorder.start` command
/// ends, however it ends (a stop, a hang-up, the session closing).
struct Registration<'a> {
  recordings: &'a Recordings,
  key: String,
  recording: Arc<Recording>,
}

impl Drop for Registration<'_> {
  fn drop(&mut self) {
    let mut table = self
      .recordings
      .by_context
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
    if table
      .get(&self.key)
      .is_some_and(|current| Arc::ptr_eq(current, &self.recording))
    {
      table.remove(&self.key);
    }
  }
}

/// The table key for a context name: the composite it resolves to, so
/// `default` and `instance:default` name the same recording.
fn key(browser: &Browser, context: &str) -> Outcome<String> {
  context_key_for(browser, context)
    .map(|key| key.to_composite())
    .map_err(|e| e.to_string())
}

fn parse_target(language: OutputLanguage, target: Option<&str>) -> Outcome<Scaffold> {
  match (language, target) {
    (OutputLanguage::TypeScript, None) => Ok(Scaffold::Test),
    (_, None) => Ok(Scaffold::Script),
    (_, Some(target)) => Scaffold::parse(target),
  }
}

impl Recordings {
  fn get(&self, browser: &Browser, context: &str) -> Outcome<Arc<Recording>> {
    let key = key(browser, context)?;
    self
      .by_context
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .get(&key)
      .cloned()
      .ok_or_else(|| format!("no recording is running on context '{context}'"))
  }

  /// Record until stopped or the client hangs up, streaming each action;
  /// answers with the whole recording.
  pub(crate) async fn start(
    &self,
    browser: &Browser,
    context: &str,
    args: serde_json::Value,
    events: &EventSink,
  ) -> Outcome<RecordingResult> {
    let request: RecorderStartRequest = parse("recorder.start", args)?;
    let language = OutputLanguage::parse_cli(request.language.as_deref().unwrap_or("ts"));
    let scaffold = parse_target(language, request.target.as_deref())?;
    let mode = request.mode.as_deref().map(RecorderMode::parse).transpose()?;
    let secrets = match request.secrets.as_deref() {
      None => Secrets::Literal,
      Some("env") => Secrets::Env,
      Some(other) => return Err(format!("unknown secrets '{other}': expected 'env'")),
    };
    let key = key(browser, context)?;

    // Claimed before anything slow happens, so two starts on one context
    // cannot both get through.
    let recording = Arc::new(Recording {
      recorder: tokio::sync::Mutex::new(None),
      stop: tokio::sync::Notify::new(),
      result: watch::channel(None).0,
      language,
      scaffold,
      title: request.title.clone(),
    });
    {
      let mut table = self.by_context.lock().unwrap_or_else(PoisonError::into_inner);
      if table.contains_key(&key) {
        return Err(format!("a recording is already running on context '{context}'"));
      }
      table.insert(key.clone(), Arc::clone(&recording));
    }
    let _registration = Registration {
      recordings: self,
      key,
      recording: Arc::clone(&recording),
    };
    let page = page(browser, context).await?;

    if let Some(url) = &request.url {
      page.goto(url).await.map_err(|e| format!("navigating to {url}: {e}"))?;
    }
    let line_emitter = recording.emitter();
    let sink_events = events.clone();
    let recorder = Recorder::attach_with(&page, secrets, move |event: RecorderEvent| match event {
      RecorderEvent::Action {
        action,
        replaces_previous,
        replaces_index,
      } => sink_events.send(EventPayload::Recorded {
        code: dedent(&line_emitter.action(&action)),
        action,
        replaces_previous,
        replaces_index,
      }),
      RecorderEvent::Mode(mode) => sink_events.send(EventPayload::RecorderMode {
        mode: mode.as_str().to_string(),
      }),
      RecorderEvent::Secret { name, value } => sink_events.send(EventPayload::Secret { name, value }),
    })
    .await
    .map_err(|e| format!("attaching the recorder: {e}"))?;
    if let Some(mode) = mode {
      recorder.set_mode(mode).await;
    }
    *recording.recorder.lock().await = Some(recorder);

    tokio::select! {
      () = recording.stop.notified() => {},
      () = events.closed() => {},
    }
    let finished = match recording.recorder.lock().await.take() {
      Some(recorder) => recorder.finish().await,
      None => ferridriver::codegen::recorder::Recorded::default(),
    };
    let result = Arc::new(RecordingResult {
      source: ferridriver::codegen::emitter::render(&*recording.emitter(), &finished.actions),
      actions: finished.actions,
      secrets: finished.secrets,
    });
    recording.result.send_replace(Some(Arc::clone(&result)));
    Ok(Arc::unwrap_or_clone(result))
  }

  /// End the context's recording and answer with what it recorded.
  pub(crate) async fn stop(&self, browser: &Browser, context: &str) -> Outcome<RecordingResult> {
    let recording = self.get(browser, context)?;
    let mut finished = recording.result.subscribe();
    recording.stop.notify_one();
    let result = finished
      .wait_for(Option::is_some)
      .await
      .map_err(|_| "the recording ended without a result".to_string())?
      .clone();
    result
      .map(Arc::unwrap_or_clone)
      .ok_or_else(|| "the recording ended without a result".to_string())
  }

  pub(crate) async fn mode(&self, browser: &Browser, context: &str, args: serde_json::Value) -> Outcome<RecorderMode> {
    let request: RecorderModeRequest = parse("recorder.mode", args)?;
    let mode = RecorderMode::parse(&request.mode)?;
    let recording = self.get(browser, context)?;
    let recorder = recording.recorder.lock().await;
    let recorder = recorder
      .as_ref()
      .ok_or_else(|| format!("the recording on context '{context}' is still starting"))?;
    recorder.set_mode(mode).await;
    Ok(mode)
  }

  pub(crate) async fn assert(
    &self,
    browser: &Browser,
    context: &str,
    args: serde_json::Value,
  ) -> Outcome<ferridriver::codegen::RecordedAction> {
    let request: RecorderAssertRequest = parse("recorder.assert", args)?;
    let recording = self.get(browser, context)?;
    let recorder = recording.recorder.lock().await;
    let recorder = recorder
      .as_ref()
      .ok_or_else(|| format!("the recording on context '{context}' is still starting"))?;
    match request.kind {
      PageAssertion::Url => Ok(recorder.assert_url().await),
      PageAssertion::Title => recorder.assert_title().await.map_err(|e| e.to_string()),
    }
  }
}

/// `code` without the indent the scaffold puts every line of its body
/// at, keeping the indent within a statement that spans lines.
fn dedent(code: &str) -> String {
  let indent = code
    .lines()
    .filter(|line| !line.trim().is_empty())
    .map(|line| line.len() - line.trim_start().len())
    .min()
    .unwrap_or(0);
  code
    .lines()
    .map(|line| line.get(indent..).unwrap_or_else(|| line.trim_start()))
    .collect::<Vec<_>>()
    .join("\n")
}

impl Recordings {
  /// Make a recorded step's value a secret.
  pub(crate) async fn secret(
    &self,
    browser: &Browser,
    context: &str,
    args: serde_json::Value,
  ) -> Outcome<serde_json::Value> {
    let request: RecorderSecretRequest = parse("recorder.secret", args)?;
    let recording = self.get(browser, context)?;
    let recorder = recording.recorder.lock().await;
    let recorder = recorder
      .as_ref()
      .ok_or_else(|| format!("the recording on context '{context}' is still starting"))?;
    let steps = recorder
      .secret(request.step, &request.name)
      .await
      .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "name": request.name, "steps": steps }))
  }
}

#[cfg(test)]
mod tests {
  use super::{dedent, typed_without_a_key};

  #[test]
  fn a_statement_keeps_its_own_indent_without_the_bodys() {
    assert_eq!(
      dedent("  await page.locator('#a').click({\n    clickCount: 3\n  });\n"),
      "await page.locator('#a').click({\n  clickCount: 3\n});"
    );
    assert_eq!(dedent("    When I click \"#a\"\n"), "When I click \"#a\"");
  }

  #[test]
  fn only_a_character_the_us_keyboard_lacks_is_typed_as_text() {
    assert!(typed_without_a_key("é"));
    assert!(!typed_without_a_key("a"));
    assert!(!typed_without_a_key("A"));
    assert!(!typed_without_a_key("Enter"));
    assert!(!typed_without_a_key("Unidentified"));
  }
}
