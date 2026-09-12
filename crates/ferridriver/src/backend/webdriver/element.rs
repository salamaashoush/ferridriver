use base64::Engine;
use serde_json::{Value, json};

use super::page::WebDriverPage;
use super::session::Command;
use crate::backend::ImageFormat;
use crate::error::{FerriError, Result};

pub struct WebDriverElement {
  pub(crate) page: WebDriverPage,
  pub(crate) id: String,
}

impl WebDriverElement {
  #[must_use]
  pub fn new(page: WebDriverPage, id: String) -> Self {
    Self { page, id }
  }

  pub async fn click(&self) -> Result<()> {
    self
      .page
      .command(Command::post(&["element", &self.id, "click"], json!({})))
      .await?;
    Ok(())
  }

  pub async fn dblclick(&self) -> Result<()> {
    self.scroll_into_view().await?;
    let (x, y) = self.center().await?;
    self.page.click_at_opts(x, y, "left", 2).await
  }

  pub async fn hover(&self) -> Result<()> {
    self.scroll_into_view().await?;
    let (x, y) = self.center().await?;
    self.page.move_mouse(x, y).await
  }

  pub async fn type_str(&self, text: &str) -> Result<()> {
    self
      .page
      .command(Command::post(&["element", &self.id, "value"], json!({"text":text})))
      .await?;
    Ok(())
  }

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

  pub async fn call_js_fn(&self, function: &str) -> Result<()> {
    self.call_js_fn_value(function).await?;
    Ok(())
  }

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

  pub async fn screenshot(&self, format: ImageFormat) -> Result<Vec<u8>> {
    let value = self
      .page
      .command(Command::get(&["element", &self.id, "screenshot"]))
      .await?;
    let encoded = value
      .as_str()
      .ok_or_else(|| FerriError::protocol("WebDriver element screenshot", "expected base64 PNG"))?;
    let png = base64::engine::general_purpose::STANDARD
      .decode(encoded)
      .map_err(|error| FerriError::protocol("WebDriver element screenshot", error.to_string()))?;
    if matches!(format, ImageFormat::Png) {
      return Ok(png);
    }
    let image = image::load_from_memory(&png).map_err(|error| FerriError::backend(error.to_string()))?;
    let mut bytes = std::io::Cursor::new(Vec::new());
    image
      .write_to(
        &mut bytes,
        match format {
          ImageFormat::Png => image::ImageFormat::Png,
          ImageFormat::Jpeg => image::ImageFormat::Jpeg,
          ImageFormat::Webp => image::ImageFormat::WebP,
        },
      )
      .map_err(|error| FerriError::backend(error.to_string()))?;
    Ok(bytes.into_inner())
  }
}
