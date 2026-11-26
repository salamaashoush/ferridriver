//! `BidiPage` screenshots, accessibility, PDF, and screencast.

use super::{
  Arc, AxNodeData, AxProperty, BackgroundColor, BidiPage, Engine, FerriError, ImageFormat, Ordering, Result,
  ScreenshotCapture, ScreenshotScale, json, to_css_pixels,
};

impl BidiPage {
  // ── Screenshots ─────────────────────────────────────────────────────────

  /// `bidiPage.ts::setBackgroundColor`: no `BiDi` command overrides the
  /// default background, so only the reset is accepted.
  ///
  /// # Errors
  ///
  /// Returns `FerriError::Unsupported` when `color` is `Some`, since no `BiDi` command overrides
  /// the default background.
  pub fn set_background_color(color: Option<BackgroundColor>) -> Result<()> {
    if color.is_some() {
      return Err(FerriError::unsupported(
        "BiDi/Firefox does not support `omitBackground` screenshots — no BiDi command exposes the transparent-background override.",
      ));
    }
    Ok(())
  }

  /// `bidiPage.ts::takeScreenshot`. `browsingContext.captureScreenshot`
  /// always captures at the device pixel ratio, and upstream lets `scale`
  /// go unhonoured there; `css` is met here by resampling the capture to
  /// the rect's CSS size, as the `WebDriver` backend does.
  ///
  /// # Errors
  ///
  /// Returns an error when `capture` has no rect, when the scroll-offset `script.evaluate` or
  /// `browsingContext.captureScreenshot` fails, or when the image data is missing or cannot be
  /// decoded, resampled or re-encoded.
  pub async fn take_screenshot(&self, capture: ScreenshotCapture) -> Result<Vec<u8>> {
    let (rect, origin) = match (capture.document_rect, capture.viewport_rect) {
      // Firefox clips a document-origin capture to the root element's
      // scroll size, which is 0 tall when the page's content is all
      // absolutely positioned, so a rect the viewport can hold is taken
      // in viewport coordinates instead.
      (Some(rect), _) if capture.fits_viewport => {
        let scroll = self
          .eval_internal(
            "JSON.stringify({ x: window.scrollX, y: window.scrollY })",
            &self.context_id,
          )
          .await?
          .and_then(|v| {
            v.as_str()
              .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
          })
          .unwrap_or_default();
        let offset = |key: &str| scroll.get(key).and_then(serde_json::Value::as_f64).unwrap_or(0.0);
        (
          crate::options::ClipRect {
            x: rect.x - offset("x"),
            y: rect.y - offset("y"),
            ..rect
          },
          "viewport",
        )
      },
      (Some(rect), _) => (rect, "document"),
      (None, Some(rect)) => (rect, "viewport"),
      (None, None) => return Err(FerriError::backend("screenshot: no rect to capture")),
    };
    let format_type = match capture.format {
      ImageFormat::Png => "image/png",
      ImageFormat::Webp => "image/webp",
      ImageFormat::Jpeg => "image/jpeg",
    };
    let mut format = json!({ "type": format_type });
    if let Some(quality) = capture.quality {
      format["quality"] = json!(f64::from(quality) / 100.0);
    }
    let result = self
      .cmd(
        "browsingContext.captureScreenshot",
        json!({
          "context": &*self.context_id,
          "format": format,
          "origin": origin,
          "clip": { "type": "box", "x": rect.x, "y": rect.y, "width": rect.width, "height": rect.height },
        }),
      )
      .await?;
    let data = result
      .get("data")
      .and_then(|v| v.as_str())
      .ok_or_else(|| FerriError::backend("Screenshot: missing data"))?;
    let bytes = base64::engine::general_purpose::STANDARD
      .decode(data)
      .map_err(|e| FerriError::Backend(format!("Screenshot base64 decode: {e}")))?;
    if capture.scale == ScreenshotScale::Css {
      return to_css_pixels(bytes, &rect, &capture);
    }
    Ok(bytes)
  }

  // ── Accessibility ───────────────────────────────────────────────────────

  /// # Errors
  ///
  /// Returns an error when injecting the selector engine or one of the `script.evaluate` calls
  /// fails or throws, or when the returned tree is not valid JSON.
  pub async fn accessibility_tree(&self) -> Result<Vec<AxNodeData>> {
    self.accessibility_tree_with_depth(-1).await
  }

  /// # Errors
  ///
  /// Returns an error when injecting the selector engine or one of the `script.evaluate` calls
  /// fails or throws, or when the returned tree is not valid JSON.
  pub async fn accessibility_tree_with_depth(&self, max_depth: i32) -> Result<Vec<AxNodeData>> {
    let fd = self.injected_script().await?;
    self
      .eval_internal(crate::selectors::AX_SUPPORT_JS, &self.context_id)
      .await?;
    // Use the shared JS accessibility tree helper from window.__fd.accessibilityTree().
    // This uses Playwright's ARIA role/name computation and tags elements with data-fdref
    // for ref resolution. Shared between BiDi and WebKit backends.
    let result = self
      .eval_internal(
        &format!("JSON.stringify({fd}.accessibilityTree({max_depth}))"),
        &self.context_id,
      )
      .await?;

    let json_str = result
      .and_then(|v| v.as_str().map(String::from))
      .unwrap_or_else(|| "[]".into());
    let arr: Vec<serde_json::Value> = serde_json::from_str(&json_str)
      .map_err(|e| FerriError::protocol("script.evaluate", format!("accessibility_tree parse: {e}")))?;

    let mut nodes = Vec::with_capacity(arr.len());
    for item in &arr {
      let mut properties = Vec::new();
      // Extract rich properties from the JS helper
      if let Some(checked) = item.get("checked").and_then(|v| v.as_str())
        && !checked.is_empty()
      {
        properties.push(AxProperty {
          name: "checked".into(),
          value: Some(serde_json::Value::String(checked.into())),
        });
      }
      if item
        .get("disabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
      {
        properties.push(AxProperty {
          name: "disabled".into(),
          value: Some(serde_json::Value::Bool(true)),
        });
      }
      if item
        .get("readonly")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
      {
        properties.push(AxProperty {
          name: "readonly".into(),
          value: Some(serde_json::Value::Bool(true)),
        });
      }
      let level = item.get("level").and_then(serde_json::Value::as_i64).unwrap_or(0);
      if level > 0 {
        properties.push(AxProperty {
          name: "level".into(),
          value: Some(serde_json::json!(level)),
        });
      }
      if let Some(expanded) = item.get("expanded").and_then(|v| v.as_str())
        && !expanded.is_empty()
      {
        properties.push(AxProperty {
          name: "expanded".into(),
          value: Some(serde_json::Value::String(expanded.into())),
        });
      }
      if item
        .get("required")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
      {
        properties.push(AxProperty {
          name: "required".into(),
          value: Some(serde_json::Value::Bool(true)),
        });
      }
      if let Some(url) = item.get("url").and_then(|v| v.as_str())
        && !url.is_empty()
      {
        properties.push(AxProperty {
          name: "url".into(),
          value: Some(serde_json::Value::String(url.into())),
        });
      }

      nodes.push(AxNodeData {
        node_id: item.get("nodeId").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        parent_id: item.get("parentId").and_then(|v| v.as_str()).map(String::from),
        backend_dom_node_id: item.get("backendId").and_then(serde_json::Value::as_i64),
        ignored: item
          .get("ignored")
          .and_then(serde_json::Value::as_bool)
          .unwrap_or(false),
        role: item.get("role").and_then(|v| v.as_str()).map(String::from),
        name: item.get("name").and_then(|v| v.as_str()).map(String::from),
        description: item.get("description").and_then(|v| v.as_str()).map(String::from),
        properties,
      });
    }
    Ok(nodes)
  }

  // ── Element screenshot ──────────────────────────────────────────────────

  // (Handled above in screenshot_element)

  // ── PDF ─────────────────────────────────────────────────────────────────

  /// Generate a PDF of the current page via `WebDriver` `BiDi`
  /// `browsingContext.print`.
  ///
  /// `BiDi`'s `PrintParameters` shape is narrower than CDP's — no header /
  /// footer template, no tagged/outline, no `preferCSSPageSize`. The
  /// canonical mapping is at
  /// `/tmp/playwright/packages/playwright-core/src/server/bidi/bidiPdf.ts`;
  /// fields unsupported by `BiDi` are silently dropped here (Playwright does
  /// the same). Unit conversion mirrors the CDP backend.
  ///
  /// # Errors
  ///
  /// Returns `FerriError::InvalidArgument` for an unknown paper `format`, and an error when
  /// `browsingContext.print` fails or its PDF data is missing or not valid base64.
  pub async fn pdf(&self, opts: crate::options::PdfOptions) -> Result<Vec<u8>> {
    let mut paper_width = 8.5_f64;
    let mut paper_height = 11.0_f64;
    if let Some(ref format) = opts.format {
      if let Some((w, h)) = crate::options::pdf_paper_format_size(format) {
        paper_width = w;
        paper_height = h;
      } else {
        return Err(FerriError::invalid_argument(
          "format",
          format!("unknown paper format: {format}"),
        ));
      }
    } else {
      if let Some(ref w) = opts.width {
        paper_width = w.to_inches();
      }
      if let Some(ref h) = opts.height {
        paper_height = h.to_inches();
      }
    }

    let margin = opts.margin.unwrap_or_default();
    let page_ranges: Option<Vec<String>> = opts
      .page_ranges
      .as_deref()
      .filter(|s| !s.is_empty())
      .map(|s| s.split(',').map(|r| r.trim().to_string()).collect());

    let mut params = json!({
      "context": &*self.context_id,
      "background": opts.print_background.unwrap_or(false),
      "margin": {
        "bottom": margin.bottom.as_ref().map_or(0.0, crate::options::PdfSize::to_inches),
        "left": margin.left.as_ref().map_or(0.0, crate::options::PdfSize::to_inches),
        "right": margin.right.as_ref().map_or(0.0, crate::options::PdfSize::to_inches),
        "top": margin.top.as_ref().map_or(0.0, crate::options::PdfSize::to_inches),
      },
      "orientation": if opts.landscape.unwrap_or(false) { "landscape" } else { "portrait" },
      "page": { "width": paper_width, "height": paper_height },
      "scale": opts.scale.unwrap_or(1.0),
    });
    if let Some(ranges) = page_ranges {
      params["pageRanges"] = serde_json::Value::Array(ranges.into_iter().map(serde_json::Value::String).collect());
    }

    let result = self.cmd("browsingContext.print", params).await?;

    let data_str = result
      .get("data")
      .and_then(|v| v.as_str())
      .ok_or_else(|| FerriError::backend("PDF: missing data"))?;
    base64::engine::general_purpose::STANDARD
      .decode(data_str)
      .map_err(|e| FerriError::Backend(format!("PDF base64 decode: {e}")))
  }

  // ── Screencast ──────────────────────────────────────────────────────────

  /// Start screencast via repeated screenshots + event-driven captures.
  /// `BiDi` has no native screencast API. We combine polling at ~15 fps with
  /// captures on navigation/load events to ensure key visual transitions
  /// are recorded even for fast tests.
  pub fn start_screencast(
    &self,
    quality: u8,
    _max_width: u32,
    _max_height: u32,
  ) -> impl std::future::Future<Output = Result<crate::backend::screencast::RawScreencast>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let session = Arc::clone(&self.session);
    let ctx_id = Arc::clone(&self.context_id);
    let closed = Arc::clone(&self.closed);

    // Subscribe to navigation events to capture frames at key moments.
    let mut event_rx = self.session.transport.subscribe_events();
    let event_notify = Arc::new(tokio::sync::Notify::new());
    let event_notify2 = Arc::clone(&event_notify);
    let event_ctx = Arc::clone(&self.context_id);

    // Tracked in `listener_tasks` (below) so page close reclaims it —
    // the session-wide subscription otherwise keeps this tap alive
    // long after the recording stopped.
    let nav_tap = tokio::spawn(async move {
      while let Some(event) = crate::events::recv_tolerant(&mut event_rx).await {
        let is_relevant = matches!(
          event.method.as_str(),
          "browsingContext.load" | "browsingContext.domContentLoaded" | "browsingContext.navigationCommitted"
        );
        if is_relevant
          && let Some(c) = event.params.get("context").and_then(|v| v.as_str())
          && c == &*event_ctx
        {
          event_notify2.notify_one();
        }
      }
    });
    let nav_tap_abort = nav_tap.abort_handle();
    self.track_listener(nav_tap.abort_handle());

    tokio::spawn(async move {
      let target_interval = std::time::Duration::from_millis(66); // ~15 fps
      let capture_params = json!({
        "context": &*ctx_id,
        "format": {"type": "image/jpeg", "quality": f64::from(quality) / 100.0},
        "origin": "viewport"
      });

      loop {
        if closed.load(Ordering::Relaxed)
          || !matches!(
            shutdown_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
          )
        {
          break;
        }
        let frame_start = tokio::time::Instant::now();

        let result = session
          .transport
          .send_command("browsingContext.captureScreenshot", capture_params.clone())
          .await;

        if let Ok(result) = result {
          if let Some(data_str) = result.get("data").and_then(|v| v.as_str())
            && let Ok(jpeg_bytes) = base64::engine::general_purpose::STANDARD.decode(data_str)
          {
            let ts = std::time::SystemTime::now()
              .duration_since(std::time::UNIX_EPOCH)
              .unwrap_or_default()
              .as_secs_f64();
            if tx.send((jpeg_bytes, ts)).is_err() {
              break;
            }
          }
        } else {
          break;
        }

        // Sleep for the remainder of the frame interval, or wake early on
        // navigation events to capture content transitions.
        let elapsed = frame_start.elapsed();
        if elapsed < target_interval {
          let remaining = target_interval.checked_sub(elapsed).unwrap_or_default();
          tokio::select! {
            () = tokio::time::sleep(remaining) => {},
            () = event_notify.notified() => {},
            _ = &mut shutdown_rx => break,
          }
        }
      }
      nav_tap_abort.abort();
    });

    std::future::ready(Ok((rx, shutdown_tx)))
  }

  pub fn stop_screencast(&self) -> impl std::future::Future<Output = Result<()>> {
    let _ = &self.context_id;
    std::future::ready(Ok(()))
  }
}
