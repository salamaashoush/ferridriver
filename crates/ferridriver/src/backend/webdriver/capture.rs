use serde_json::Value;

use super::page::WebDriverPage;
use crate::backend::{AxNodeData, AxProperty, ImageFormat, ScreenshotOpts, ScreenshotScale};
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

  pub async fn screenshot(&self, opts: ScreenshotOpts) -> Result<Vec<u8>> {
    if opts.full_page || opts.omit_background {
      return Err(FerriError::unsupported(
        "Classic WebDriver screenshots capture the visible viewport and do not expose full-page or transparent-background capture",
      ));
    }
    let geometry = self
      .execute_script("return {width:innerWidth,height:innerHeight};", vec![])
      .await?;
    let result = async {
      let css = crate::backend::screenshot_js::build_css(&opts);
      if !css.is_empty() {
        self
          .execute_script(&crate::backend::screenshot_js::install_style_js(&css), vec![])
          .await?;
      }
      if let Some(mask) = crate::backend::screenshot_js::install_mask_js(&opts) {
        self.execute_script(&mask, vec![]).await?;
      }
      self.screenshot_png().await
    }
    .await;
    let cleanup = self
      .execute_script(
        &format!(
          "{};{}",
          crate::backend::screenshot_js::uninstall_mask_js(),
          crate::backend::screenshot_js::uninstall_style_js()
        ),
        vec![],
      )
      .await;
    let bytes = match (result, cleanup) {
      (Ok(bytes), Ok(_)) => bytes,
      (Err(error), Ok(_)) | (Ok(_), Err(error)) => return Err(error),
      (Err(error), Err(cleanup)) => {
        return Err(FerriError::backend(format!(
          "{error}; screenshot cleanup failed: {cleanup}"
        )));
      },
    };
    let mut image = image::load_from_memory(&bytes).map_err(|error| FerriError::backend(error.to_string()))?;
    let viewport_width = geometry["width"]
      .as_f64()
      .filter(|width| *width > 0.0)
      .ok_or_else(|| FerriError::protocol("screenshot", "invalid viewport width"))?;
    let ratio = f64::from(image.width()) / viewport_width;
    if let Some(clip) = opts.clip {
      let x = pixels((clip.x * ratio).floor())?;
      let y = pixels((clip.y * ratio).floor())?;
      let width = pixels((clip.width * ratio).ceil())?;
      let height = pixels((clip.height * ratio).ceil())?;
      if width == 0
        || height == 0
        || x.saturating_add(width) > image.width()
        || y.saturating_add(height) > image.height()
      {
        return Err(FerriError::invalid_argument(
          "clip",
          "screenshot clip must fit within the visible viewport",
        ));
      }
      image = image.crop_imm(x, y, width, height);
    }
    if opts.scale == Some(ScreenshotScale::Css) && (ratio - 1.0).abs() > f64::EPSILON {
      image = image.resize_exact(
        pixels((f64::from(image.width()) / ratio).round())?,
        pixels((f64::from(image.height()) / ratio).round())?,
        image::imageops::FilterType::Lanczos3,
      );
    }
    let mut output = std::io::Cursor::new(Vec::new());
    if matches!(opts.format, ImageFormat::Jpeg) {
      let quality = u8::try_from(opts.quality.unwrap_or(80))
        .ok()
        .filter(|quality| *quality <= 100)
        .ok_or_else(|| FerriError::invalid_argument("quality", "expected an integer from 0 to 100"))?;
      image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, quality)
        .encode_image(&image.to_rgb8())
        .map_err(|error| FerriError::backend(error.to_string()))?;
    } else {
      image
        .write_to(
          &mut output,
          if matches!(opts.format, ImageFormat::Webp) {
            image::ImageFormat::WebP
          } else {
            image::ImageFormat::Png
          },
        )
        .map_err(|error| FerriError::backend(error.to_string()))?;
    }
    Ok(output.into_inner())
  }

  pub async fn screenshot_element(&self, selector: &str, format: ImageFormat) -> Result<Vec<u8>> {
    self.find_element(selector).await?.screenshot(format).await
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
