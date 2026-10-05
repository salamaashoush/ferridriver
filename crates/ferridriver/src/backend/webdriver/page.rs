use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use super::session::{Command, Target, WebDriverSession};
use crate::error::{FerriError, Result};
use crate::operation_budget::OperationBudget;

#[derive(Clone)]
pub struct WebDriverPage {
  pub(crate) session: Arc<WebDriverSession>,
  pub(crate) target: Target,
  pub(crate) timeout_ms: u64,
  closed: Arc<AtomicBool>,
  pub events: crate::events::EventEmitter,
  pub(crate) page_backref: crate::backend::PageBackref,
  pub(crate) frame_cache: Arc<std::sync::Mutex<crate::frame_cache::FrameCache>>,
  pub(crate) observed: Arc<std::sync::Mutex<crate::observed::ObservedBuffers>>,
  pub(crate) frame_listener_started: Arc<AtomicBool>,
  pub(crate) dialog_manager: crate::dialog::DialogManager,
  pub(crate) file_chooser_manager: crate::file_chooser::FileChooserManager,
  pub(crate) download_manager: crate::download::DownloadManager,
  pub(crate) capabilities: Arc<Value>,
}

impl WebDriverPage {
  #[must_use]
  pub fn new(session: Arc<WebDriverSession>, target: Target, timeout_ms: u64) -> Self {
    Self {
      session,
      target,
      timeout_ms,
      closed: Arc::new(AtomicBool::new(false)),
      events: crate::events::EventEmitter::new(),
      page_backref: crate::backend::PageBackref::new(),
      frame_cache: Arc::default(),
      observed: Arc::default(),
      frame_listener_started: Arc::default(),
      dialog_manager: crate::dialog::DialogManager::new(),
      file_chooser_manager: crate::file_chooser::FileChooserManager::new(),
      download_manager: crate::download::DownloadManager::new(),
      capabilities: Arc::new(Value::Null),
    }
  }

  pub async fn command(&self, command: Command) -> Result<Value> {
    self.ensure_open()?;
    self
      .session
      .execute(self.target.clone(), command, self.timeout_ms)
      .await
  }

  pub async fn navigate(&self, url: &str, timeout_ms: u64) -> Result<()> {
    self.ensure_open()?;
    if super::capabilities::uses_xcuitest(&self.capabilities)
      && self.capabilities["browserName"]
        .as_str()
        .is_some_and(|name| name.eq_ignore_ascii_case("safari"))
      && let Ok(url) = reqwest::Url::parse(url)
      && url.scheme() == "data"
    {
      return self
        .session
        .native_navigate(self.target.clone(), url.as_str(), timeout_ms)
        .await;
    }
    self
      .session
      .execute_sequence(
        self.target.clone(),
        vec![
          Command::post(
            &["timeouts"],
            json!({"pageLoad":if timeout_ms == 0 { Value::Null } else { json!(timeout_ms) }}),
          ),
          Command::post(&["url"], json!({"url":url})),
        ],
        timeout_ms,
      )
      .await?;
    Ok(())
  }

  pub async fn evaluate(&self, expression: &str) -> Result<Option<Value>> {
    let value = self.execute_script(&format!("return ({expression});"), vec![]).await?;
    Ok(Some(value))
  }

  pub async fn execute_script(&self, script: &str, arguments: Vec<Value>) -> Result<Value> {
    if super::capabilities::uses_xcuitest(&self.capabilities) {
      return self.execute_callback_script(script, arguments).await;
    }
    self.ensure_open()?;
    let mut commands = Vec::with_capacity(2);
    if OperationBudget::capture().is_some() {
      commands.push(Command::post(
        &["timeouts"],
        json!({"script": if self.timeout_ms == 0 { Value::Null } else { json!(self.timeout_ms) }}),
      ));
    }
    commands.push(Command::post(
      &["execute", "sync"],
      json!({"script":script,"args":arguments}),
    ));
    self
      .session
      .execute_sequence(self.target.clone(), commands, self.timeout_ms)
      .await
  }

  async fn execute_callback_script(&self, script: &str, arguments: Vec<Value>) -> Result<Value> {
    self.ensure_open()?;
    let budget = OperationBudget::current(self.timeout_ms)?;
    let Some(timeout_ms) = budget
      .remaining_ms()
      .map_err(|error| error.error("executing XCUITest script"))?
    else {
      return Err(FerriError::unsupported(
        "XCUITest requires a finite asynchronous script timeout",
      ));
    };
    let atom_timeout = self
      .capabilities
      .get("appium:webviewAtomWaitTimeout")
      .or_else(|| self.capabilities.get("webviewAtomWaitTimeout"))
      .or_else(|| self.capabilities.pointer("/appium:options/webviewAtomWaitTimeout"))
      .and_then(Value::as_u64)
      .filter(|timeout| *timeout != 0)
      .unwrap_or(120_000);
    if timeout_ms > atom_timeout {
      return Err(FerriError::unsupported(format!(
        "XCUITest's webviewAtomWaitTimeout is {atom_timeout}ms; configure a larger provider capability before requesting this script budget"
      )));
    }
    // XCUITest's Selenium atoms do not await promises returned from execute/sync.
    let script = format!(
      "const args=Array.from(arguments); const done=args.pop();\n\
       Promise.resolve().then(()=>(function(){{{script}\n}}).apply(null,args)).then(\
       value=>done({{ok:true,value}}), error=>done({{ok:false,error:String(error)}}));"
    );
    let result = self
      .session
      .execute_sequence(
        self.target.clone(),
        vec![
          Command::post(&["timeouts"], json!({"script":timeout_ms})),
          Command::post(&["execute", "async"], json!({"script":script,"args":arguments})),
        ],
        self.timeout_ms,
      )
      .await?;
    match result["ok"].as_bool() {
      Some(true) => Ok(result["value"].clone()),
      Some(false) => {
        Err(FerriError::evaluation(result["error"].as_str().ok_or_else(|| {
          FerriError::protocol("execute async", "missing script error")
        })?))
      },
      None => match result.pointer("/error/message").and_then(Value::as_str) {
        Some(message) => Err(self.session.protocol_error("Runtime.awaitPromise", message)),
        None => Err(FerriError::protocol("execute async", "invalid callback response")),
      },
    }
  }

  pub async fn find_element(&self, selector: &str) -> Result<super::element::WebDriverElement> {
    self.ensure_engine_injected().await?;
    let expression = crate::selectors::build_selone_js(selector, "window.__fd", false)?;
    let value = self.execute_script(&format!("return ({expression});"), vec![]).await?;
    if value.is_null() {
      return Err(FerriError::invalid_selector(selector, "not found"));
    }
    self.element_from_value(&value)
  }

  pub async fn evaluate_to_element(&self, expression: &str) -> Result<super::element::WebDriverElement> {
    let value = self.execute_script(&format!("return ({expression});"), vec![]).await?;
    self.element_from_value(&value)
  }

  fn element_from_value(&self, value: &Value) -> Result<super::element::WebDriverElement> {
    let id = value
      .get("element-6066-11e4-a52e-4f735466cecf")
      .and_then(Value::as_str)
      .ok_or_else(|| FerriError::protocol("WebDriver element", "expression did not resolve to an element"))?;
    Ok(super::element::WebDriverElement::new(self.clone(), id.to_owned()))
  }

  pub async fn child_frame(&self, selector: &str) -> Result<Self> {
    let element = self.find_element(selector).await?;
    let mut page = self.clone();
    page
      .target
      .frames
      .push(json!({"element-6066-11e4-a52e-4f735466cecf":element.id}));
    page.execute_script("return document.readyState;", vec![]).await?;
    Ok(page)
  }

  pub async fn screenshot_png(&self) -> Result<Vec<u8>> {
    use base64::Engine;
    let value = self.command(Command::get(&["screenshot"])).await?;
    let encoded = value
      .as_str()
      .ok_or_else(|| FerriError::protocol("WebDriver screenshot", "expected base64 PNG"))?;
    base64::engine::general_purpose::STANDARD
      .decode(encoded)
      .map_err(|error| FerriError::protocol("WebDriver screenshot", error.to_string()))
  }

  pub async fn close(&self) -> Result<()> {
    if self.is_closed() {
      return Ok(());
    }
    self
      .command(Command {
        method: reqwest::Method::DELETE,
        path: vec!["window".into()],
        body: None,
      })
      .await?;
    self.dispose_local();
    Ok(())
  }

  #[must_use]
  pub fn is_closed(&self) -> bool {
    self.closed.load(Ordering::Acquire)
  }

  pub(crate) fn dispose_local(&self) {
    if !self.closed.swap(true, Ordering::AcqRel) {
      self.events.emit(crate::events::PageEvent::Close);
    }
  }

  pub(super) fn ensure_open(&self) -> Result<()> {
    if self.is_closed() {
      return Err(FerriError::target_closed(Some("WebDriver window is closed".into())));
    }
    Ok(())
  }

  pub async fn title(&self) -> Result<Option<String>> {
    let value = self.command(Command::get(&["title"])).await?;
    value
      .as_str()
      .map(|s| Some(s.to_owned()))
      .ok_or_else(|| FerriError::protocol("WebDriver title", "expected a string"))
  }

  pub async fn url(&self) -> Result<Option<String>> {
    let value = self.command(Command::get(&["url"])).await?;
    value
      .as_str()
      .map(|s| Some(s.to_owned()))
      .ok_or_else(|| FerriError::protocol("WebDriver url", "expected a string"))
  }

  pub async fn content(&self) -> Result<String> {
    let value = self.command(Command::get(&["source"])).await?;
    value
      .as_str()
      .map(str::to_owned)
      .ok_or_else(|| FerriError::protocol("WebDriver source", "expected a string"))
  }

  pub async fn set_content(&self, html: &str) -> Result<()> {
    self
      .execute_script(
        "document.open(); document.write(arguments[0]); document.close();",
        vec![json!(html)],
      )
      .await?;
    Ok(())
  }

  pub async fn ensure_engine_injected(&self) -> Result<()> {
    self
      .execute_script(&crate::selectors::build_lazy_inject_js(), vec![])
      .await?;
    Ok(())
  }

  pub async fn injected_script(&self) -> Result<String> {
    self.ensure_engine_injected().await?;
    Ok("window.__fd".into())
  }

  pub async fn perform_actions(&self, mut actions: Value) -> Result<()> {
    self.ensure_open()?;
    if super::capabilities::uses_xcuitest(&self.capabilities) {
      self.resolve_web_action_origins(&mut actions).await?;
      self
        .session
        .native_input_command(
          self.target.clone(),
          Command::post(&["actions"], json!({"actions":actions["actions"]})),
          self.timeout_ms,
        )
        .await?;
      return Ok(());
    }
    self
      .command(Command::post(&["actions"], json!({"actions":actions["actions"]})))
      .await?;
    Ok(())
  }

  async fn resolve_web_action_origins(&self, actions: &mut Value) -> Result<()> {
    let sources = actions["actions"]
      .as_array_mut()
      .ok_or_else(|| FerriError::invalid_argument("actions", "expected action sources"))?;
    for source in sources {
      if source["type"] == "wheel" {
        return Err(FerriError::unsupported(
          "XCUITest supports touch scrolling but not wheel input",
        ));
      }
      if source["type"] != "pointer" {
        continue;
      }
      let steps = source["actions"]
        .as_array_mut()
        .ok_or_else(|| FerriError::invalid_argument("actions", "expected pointer actions"))?;
      for step in steps {
        if matches!(step["type"].as_str(), Some("pointerDown" | "pointerUp")) && step["button"] != 0 {
          return Err(FerriError::unsupported(
            "XCUITest touch input has no secondary mouse buttons",
          ));
        }
        if step["type"] != "pointerMove" || !(step["origin"].is_null() || step["origin"] == "viewport") {
          continue;
        }
        let point = self
          .session
          .native_pointer_position(
            self.target.clone(),
            coordinate(step, "x")?,
            coordinate(step, "y")?,
            self.timeout_ms,
          )
          .await?;
        step["x"] = point["x"].clone();
        step["y"] = point["y"].clone();
        step["origin"] = json!("viewport");
      }
    }
    Ok(())
  }

  pub async fn click_at(&self, x: f64, y: f64) -> Result<()> {
    self.perform_actions(crate::backend::bidi::input::click("", x, y)).await
  }

  pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<()> {
    self
      .perform_actions(crate::backend::bidi::input::click_button(
        "",
        x,
        y,
        crate::backend::bidi::input::button_name_to_id(button),
        click_count,
      ))
      .await
  }

  pub async fn click_at_with(&self, x: f64, y: f64, args: &crate::backend::BackendClickArgs) -> Result<()> {
    self
      .perform_actions(crate::backend::bidi::input::click_with_args("", x, y, args))
      .await
  }

  pub async fn hover_at_with(&self, x: f64, y: f64, args: &crate::backend::BackendHoverArgs) -> Result<()> {
    self
      .perform_actions(crate::backend::bidi::input::hover_with_args("", x, y, *args))
      .await
  }

  pub async fn tap_at_with(&self, x: f64, y: f64, _args: &crate::backend::BackendTapArgs) -> Result<()> {
    self.perform_actions(crate::backend::bidi::input::tap("", x, y)).await
  }

  pub async fn press_modifiers(&self, modifiers: &[crate::options::Modifier]) -> Result<()> {
    if modifiers.is_empty() {
      return Ok(());
    }
    self
      .perform_actions(crate::backend::bidi::input::modifiers_down("", modifiers))
      .await
  }

  pub async fn release_modifiers(&self, modifiers: &[crate::options::Modifier]) -> Result<()> {
    if modifiers.is_empty() {
      return Ok(());
    }
    self
      .perform_actions(crate::backend::bidi::input::modifiers_up("", modifiers))
      .await
  }

  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<()> {
    self
      .perform_actions(crate::backend::bidi::input::pointer_move("", x, y))
      .await
  }

  pub async fn move_mouse_smooth(&self, from_x: f64, from_y: f64, to_x: f64, to_y: f64, steps: u32) -> Result<()> {
    self
      .perform_actions(crate::backend::bidi::input::pointer_move_smooth(
        "", from_x, from_y, to_x, to_y, steps,
      ))
      .await
  }

  pub async fn mouse_wheel(&self, x: f64, y: f64) -> Result<()> {
    self
      .perform_actions(crate::backend::bidi::input::wheel_scroll("", x, y))
      .await
  }

  pub async fn mouse_down(&self, x: f64, y: f64, button: &str) -> Result<()> {
    self
      .perform_actions(crate::backend::bidi::input::mouse_down(
        "",
        x,
        y,
        crate::backend::bidi::input::button_name_to_id(button),
      ))
      .await
  }

  pub async fn mouse_up(&self, x: f64, y: f64, button: &str) -> Result<()> {
    self
      .perform_actions(crate::backend::bidi::input::mouse_up(
        "",
        x,
        y,
        crate::backend::bidi::input::button_name_to_id(button),
      ))
      .await
  }

  pub async fn click_and_drag(&self, from: (f64, f64), to: (f64, f64), steps: u32) -> Result<()> {
    self
      .perform_actions(crate::backend::bidi::input::click_and_drag("", from, to, steps))
      .await
  }

  pub async fn key_down(&self, key: &str) -> Result<()> {
    self
      .perform_actions(crate::backend::bidi::input::key_down("", key))
      .await
  }

  pub async fn key_up(&self, key: &str) -> Result<()> {
    self.perform_actions(crate::backend::bidi::input::key_up("", key)).await
  }

  pub async fn press_key(&self, key: &str) -> Result<()> {
    self
      .perform_actions(crate::backend::bidi::input::press_key("", key))
      .await
  }

  pub async fn type_str(&self, text: &str) -> Result<()> {
    self.execute_script("const el = document.activeElement; if (!el) throw new Error('No focused element'); document.execCommand('insertText', false, arguments[0]);", vec![json!(text)]).await?;
    Ok(())
  }

  pub async fn clear_cookies(&self) -> Result<()> {
    self
      .command(Command {
        method: reqwest::Method::DELETE,
        path: vec!["cookie".into()],
        body: None,
      })
      .await?;
    Ok(())
  }
}

fn coordinate(value: &Value, key: &str) -> Result<f64> {
  value[key]
    .as_f64()
    .filter(|number| number.is_finite())
    .ok_or_else(|| FerriError::protocol("XCUITest coordinate translation", format!("missing or invalid {key}")))
}
