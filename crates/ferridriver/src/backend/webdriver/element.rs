use serde_json::{Value, json};

use super::page::WebDriverPage;
use super::session::Command;
use crate::error::{FerriError, Result};

pub struct WebDriverElement {
  pub(crate) page: WebDriverPage,
  pub(crate) id: String,
}

impl WebDriverElement {
  /// # Errors
  ///
  /// Returns an error when the `WebDriver` `POST /session/{id}/element/{element id}/value` command
  /// (or `/clear` when `paths` is empty) fails, for example when the element reference is stale.
  pub async fn set_input_files(&self, paths: &[String]) -> Result<()> {
    let (command, body) = if paths.is_empty() {
      ("clear", json!({}))
    } else {
      ("value", json!({"text":paths.join("\n")}))
    };
    self
      .page
      .command(Command::post(&["element", &self.id, command], body))
      .await?;
    Ok(())
  }

  #[must_use]
  pub fn new(page: WebDriverPage, id: String) -> Self {
    Self { page, id }
  }

  /// # Errors
  ///
  /// Returns an error when the `WebDriver` `POST /session/{id}/element/{element id}/click` command
  /// fails, for example when the element is stale or obscured. On `XCUITest`, forwards the error from
  /// scrolling the element into view, reading its centre, or the pointer actions that tap it.
  pub async fn click(&self) -> Result<()> {
    if super::capabilities::uses_xcuitest(&self.page.capabilities) {
      self.scroll_into_view().await?;
      let (x, y) = self.center().await?;
      return self.page.click_at(x, y).await;
    }
    self
      .page
      .command(Command::post(&["element", &self.id, "click"], json!({})))
      .await?;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error when scrolling the element into view or reading its centre through the
  /// `WebDriver` `POST /session/{id}/execute/sync` command fails, or when the `WebDriver`
  /// `POST /session/{id}/actions` command for the double click fails.
  pub async fn dblclick(&self) -> Result<()> {
    self.scroll_into_view().await?;
    let (x, y) = self.center().await?;
    self.page.click_at_opts(x, y, "left", 2).await
  }

  /// # Errors
  ///
  /// Returns an error when scrolling the element into view or reading its centre through the
  /// `WebDriver` `POST /session/{id}/execute/sync` command fails, or when the `WebDriver`
  /// `POST /session/{id}/actions` command that moves the pointer fails.
  pub async fn hover(&self) -> Result<()> {
    self.scroll_into_view().await?;
    let (x, y) = self.center().await?;
    self.page.move_mouse(x, y).await
  }

  /// # Errors
  ///
  /// Returns an error when the `WebDriver` `POST /session/{id}/element/{element id}/value` command
  /// fails, for example when the element is stale or not interactable.
  pub async fn type_str(&self, text: &str) -> Result<()> {
    self
      .page
      .command(Command::post(&["element", &self.id, "value"], json!({"text":text})))
      .await?;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error when the `WebDriver` `POST /session/{id}/execute/sync` command fails, for
  /// example when the element reference is stale, and `FerriError::Evaluation` when the function
  /// throws.
  pub async fn call_js_fn_value(&self, function: &str) -> Result<Option<Value>> {
    let value = self
      .page
      .execute_script(
        &format!("return ({function}).call(arguments[0], arguments[0]);"),
        vec![json!({"element-6066-11e4-a52e-4f735466cecf":self.id})],
      )
      .await?;
    Ok(Some(value))
  }

  /// # Errors
  ///
  /// Forwards the error from `call_js_fn_value`.
  pub async fn call_js_fn(&self, function: &str) -> Result<()> {
    self.call_js_fn_value(function).await?;
    Ok(())
  }

  /// # Errors
  ///
  /// Forwards the error from `call_js_fn` when the `WebDriver` `POST /session/{id}/execute/sync`
  /// command that scrolls the element fails.
  pub async fn scroll_into_view(&self) -> Result<()> {
    self
      .call_js_fn("el => el.scrollIntoView({block:'center',inline:'center'})")
      .await
  }

  async fn center(&self) -> Result<(f64, f64)> {
    let rect = self
      .call_js_fn_value("el => {const r=el.getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2}}")
      .await?
      .ok_or_else(|| FerriError::protocol("WebDriver element rectangle", "missing rectangle"))?;
    let x = rect["x"]
      .as_f64()
      .ok_or_else(|| FerriError::protocol("WebDriver element rectangle", "missing x"))?;
    let y = rect["y"]
      .as_f64()
      .ok_or_else(|| FerriError::protocol("WebDriver element rectangle", "missing y"))?;
    Ok((x, y))
  }
}
