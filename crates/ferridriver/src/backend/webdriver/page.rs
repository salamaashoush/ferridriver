use std::sync::Arc;

use serde_json::{Value, json};

use super::session::{Command, Target, WebDriverSession};
use crate::error::{FerriError, Result};

#[derive(Clone)]
pub struct WebDriverPage {
  pub(crate) session: Arc<WebDriverSession>,
  pub(crate) target: Target,
  pub(crate) timeout_ms: u64,
}

impl WebDriverPage {
  #[must_use]
  pub fn new(session: Arc<WebDriverSession>, target: Target, timeout_ms: u64) -> Self {
    Self {
      session,
      target,
      timeout_ms,
    }
  }

  pub async fn command(&self, command: Command) -> Result<Value> {
    self
      .session
      .execute(self.target.clone(), command, self.timeout_ms)
      .await
  }

  pub async fn evaluate(&self, expression: &str) -> Result<Option<Value>> {
    let value = self
      .command(Command::post(
        &["execute", "sync"],
        json!({
          "script": format!("return ({expression});"), "args": []
        }),
      ))
      .await?;
    Ok(Some(value))
  }

  pub async fn execute_script(&self, script: &str, arguments: Vec<Value>) -> Result<Value> {
    self
      .command(Command::post(
        &["execute", "sync"],
        json!({"script":script,"args":arguments}),
      ))
      .await
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

  pub async fn perform_actions(&self, actions: Value) -> Result<()> {
    self
      .command(Command::post(&["actions"], json!({"actions":actions["actions"]})))
      .await?;
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
    self
      .perform_actions(crate::backend::bidi::input::modifiers_down("", modifiers))
      .await
  }

  pub async fn release_modifiers(&self, modifiers: &[crate::options::Modifier]) -> Result<()> {
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
