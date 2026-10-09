use serde_json::Value;

use super::page::WebDriverPage;
use crate::backend::{AxNodeData, AxProperty, BackgroundColor, ImageFormat, ScreenshotCapture, ScreenshotScale};
use crate::error::{FerriError, Result};

impl WebDriverPage {
  pub async fn pdf(&self, opts: crate::options::PdfOptions) -> Result<Vec<u8>> {
    use base64::Engine;
    if opts.display_header_footer == Some(true)
      || opts.header_template.is_some()
      || opts.footer_template.is_some()
      || opts.prefer_css_page_size == Some(true)
      || opts.outline == Some(true)
      || opts.tagged == Some(true)
    {
      return Err(FerriError::unsupported(
        "Classic WebDriver print does not expose header templates, CSS paper sizing, outlines, or tagged-PDF controls",
      ));
    }
    let (width, height) = match opts.format.as_deref() {
      Some(format) => crate::options::pdf_paper_format_size(format)
        .ok_or_else(|| FerriError::invalid_argument("format", format!("unknown paper format: {format}")))?,
      None => (
        opts.width.as_ref().map_or(8.5, crate::options::PdfSize::to_inches),
        opts.height.as_ref().map_or(11.0, crate::options::PdfSize::to_inches),
      ),
    };
    let margin = opts.margin.unwrap_or_default();
    let ranges: Vec<&str> = opts
      .page_ranges
      .as_deref()
      .unwrap_or_default()
      .split(',')
      .map(str::trim)
      .filter(|range| !range.is_empty())
      .collect();
    let value = self
      .command(super::session::Command::post(
        &["print"],
        serde_json::json!({
          "page":{"width":width * 2.54,"height":height * 2.54},
          "margin":{
            "top":margin.top.as_ref().map_or(0.0,crate::options::PdfSize::to_inches) * 2.54,
            "bottom":margin.bottom.as_ref().map_or(0.0,crate::options::PdfSize::to_inches) * 2.54,
            "left":margin.left.as_ref().map_or(0.0,crate::options::PdfSize::to_inches) * 2.54,
            "right":margin.right.as_ref().map_or(0.0,crate::options::PdfSize::to_inches) * 2.54,
          },
          "scale":opts.scale.unwrap_or(1.0), "background":opts.print_background.unwrap_or(false),
          "orientation":if opts.landscape.unwrap_or(false) { "landscape" } else { "portrait" },
          "pageRanges":ranges,
        }),
      ))
      .await?;
    let encoded = value
      .as_str()
      .ok_or_else(|| FerriError::protocol("print", "expected base64 PDF"))?;
    base64::engine::general_purpose::STANDARD
      .decode(encoded)
      .map_err(|error| FerriError::protocol("print", error.to_string()))
  }

  /// Classic `WebDriver` has no background override.
  pub fn set_background_color(color: Option<BackgroundColor>) -> Result<()> {
    if color.is_some() {
      return Err(FerriError::unsupported(
        "Classic WebDriver screenshots do not expose transparent-background capture",
      ));
    }
    Ok(())
  }

  /// Classic `WebDriver` captures the visible viewport only, so the rect is
  /// cropped out of that capture and anything beyond it is refused.
  pub async fn take_screenshot(&self, capture: ScreenshotCapture) -> Result<Vec<u8>> {
    let geometry = self
      .execute_script(
        "return {width:innerWidth,height:innerHeight,scrollX:scrollX,scrollY:scrollY};",
        vec![],
      )
      .await?;
    let number = |key: &str| geometry[key].as_f64().unwrap_or(0.0);
    let rect = match (capture.document_rect, capture.viewport_rect) {
      (_, Some(rect)) => rect,
      (Some(rect), None) => crate::options::ClipRect {
        x: rect.x - number("scrollX"),
        y: rect.y - number("scrollY"),
        width: rect.width,
        height: rect.height,
      },
      (None, None) => return Err(FerriError::backend("screenshot: no rect to capture")),
    };
    let beyond = !capture.fits_viewport
      || rect.x < 0.0
      || rect.y < 0.0
      || rect.x + rect.width > number("width")
      || rect.y + rect.height > number("height");
    if beyond {
      return Err(FerriError::unsupported(
        "Classic WebDriver screenshots capture the visible viewport and cannot capture beyond it",
      ));
    }
    let bytes = self.screenshot_png().await?;
    let mut image = image::load_from_memory(&bytes).map_err(|error| FerriError::backend(error.to_string()))?;
    let viewport_width = Some(number("width"))
      .filter(|width| *width > 0.0)
      .ok_or_else(|| FerriError::protocol("screenshot", "invalid viewport width"))?;
    let ratio = f64::from(image.width()) / viewport_width;
    let x = pixels((rect.x * ratio).floor())?;
    let y = pixels((rect.y * ratio).floor())?;
    let width = pixels((rect.width * ratio).ceil())?.min(image.width().saturating_sub(x));
    let height = pixels((rect.height * ratio).ceil())?.min(image.height().saturating_sub(y));
    if width == 0 || height == 0 {
      return Err(FerriError::invalid_argument(
        "clip",
        "Clipped area is either empty or outside the resulting image",
      ));
    }
    image = image.crop_imm(x, y, width, height);
    if capture.scale == ScreenshotScale::Css && (ratio - 1.0).abs() > f64::EPSILON {
      image = image.resize_exact(
        pixels((f64::from(image.width()) / ratio).round())?,
        pixels((f64::from(image.height()) / ratio).round())?,
        image::imageops::FilterType::Lanczos3,
      );
    }
    let mut output = std::io::Cursor::new(Vec::new());
    match capture.format {
      ImageFormat::Jpeg => {
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, capture.quality.unwrap_or(80))
          .encode_image(&image.to_rgb8())
          .map_err(|error| FerriError::backend(error.to_string()))?;
      },
      ImageFormat::Png | ImageFormat::Webp => {
        image
          .write_to(&mut output, image::ImageFormat::Png)
          .map_err(|error| FerriError::backend(error.to_string()))?;
      },
    }
    if capture.format == ImageFormat::Webp {
      return crate::backend::encode_webp_from_png(output.get_ref(), capture.quality);
    }
    Ok(output.into_inner())
  }

  pub async fn accessibility_tree(&self) -> Result<Vec<AxNodeData>> {
    self.accessibility_tree_with_depth(-1).await
  }

  pub async fn accessibility_tree_with_depth(&self, depth: i32) -> Result<Vec<AxNodeData>> {
    self.ensure_engine_injected().await?;
    self.execute_script(crate::selectors::AX_SUPPORT_JS, vec![]).await?;
    let values = self
      .execute_script(
        "return window.__fd.accessibilityTree(arguments[0]);",
        vec![serde_json::json!(depth)],
      )
      .await?;
    let values = values
      .as_array()
      .ok_or_else(|| FerriError::protocol("accessibility tree", "expected node array"))?;
    values.iter().map(ax_node).collect()
  }
}

fn pixels(value: f64) -> Result<u32> {
  value
    .to_string()
    .parse()
    .map_err(|_| FerriError::invalid_argument("screenshot", "invalid pixel dimension"))
}

fn ax_node(item: &Value) -> Result<AxNodeData> {
  let mut properties = Vec::new();
  for name in [
    "checked", "disabled", "readonly", "level", "expanded", "required", "url",
  ] {
    if let Some(value) = item.get(name)
      && !value.is_null()
      && value != ""
      && value != false
      && value != 0
    {
      properties.push(AxProperty {
        name: name.into(),
        value: Some(value.clone()),
      });
    }
  }
  Ok(AxNodeData {
    node_id: item["nodeId"]
      .as_str()
      .ok_or_else(|| FerriError::protocol("accessibility tree", "missing node id"))?
      .into(),
    parent_id: item["parentId"].as_str().map(str::to_owned),
    backend_dom_node_id: item["backendId"].as_i64(),
    ignored: item["ignored"].as_bool().unwrap_or(false),
    role: item["role"].as_str().map(str::to_owned),
    name: item["name"].as_str().map(str::to_owned),
    description: item["description"].as_str().map(str::to_owned),
    properties,
  })
}
