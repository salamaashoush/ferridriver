//! `CdpPage` screenshots, screencast, PDF export, and accessibility tree.

use super::*;

impl CdpPage {
  // ---- Screenshots ----

  /// `crPage.ts::setBackgroundColor`.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Emulation.setDefaultBackgroundColorOverride`, the target has closed,
  /// or the command outlives its time budget.
  pub async fn set_background_color(&self, color: Option<BackgroundColor>) -> Result<()> {
    let params = match color {
      Some(c) => serde_json::json!({ "color": { "r": c.r, "g": c.g, "b": c.b, "a": c.a } }),
      None => serde_json::json!({}),
    };
    self.cmd("Emulation.setDefaultBackgroundColorOverride", params).await?;
    Ok(())
  }

  /// `crPage.ts::takeScreenshot`.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Page.getLayoutMetrics`, `Runtime.evaluate`, `Page.captureScreenshot`
  /// or `Emulation.setTouchEmulationEnabled`, the target has closed, or the command outlives its time budget.
  /// Also fails when there is no rectangle to capture, the paint wait throws, or the answer carries no valid
  /// base64 image.
  pub async fn take_screenshot(&self, capture: ScreenshotCapture) -> Result<Vec<u8>> {
    let metrics = self.cmd("Page.getLayoutMetrics", super::super::empty_params()).await?;
    let metric = |object: &str, key: &str| {
      metrics
        .get(object)
        .and_then(|v| v.get(key))
        .and_then(serde_json::Value::as_f64)
    };
    let page_scale = metric("visualViewport", "scale").unwrap_or(1.0);
    let document_rect = match (capture.document_rect, capture.viewport_rect) {
      (Some(rect), _) => rect,
      (None, Some(viewport)) => crate::options::ClipRect {
        x: metric("visualViewport", "pageX").unwrap_or(0.0) + viewport.x,
        y: metric("visualViewport", "pageY").unwrap_or(0.0) + viewport.y,
        width: (viewport.width / page_scale + 1e-3).floor(),
        height: (viewport.height / page_scale + 1e-3).floor(),
      },
      (None, None) => return Err(FerriError::backend("screenshot: no rect to capture")),
    };
    // A document rect is laid out content, so the current page scale
    // does not apply to it.
    let mut clip_scale = if capture.viewport_rect.is_some() {
      page_scale
    } else {
      1.0
    };
    if capture.scale == ScreenshotScale::Css {
      // The deviceScaleFactor override does not reach the layout
      // metrics, so an emulated one wins over the computed ratio.
      let overridden = self
        .last_metrics_params
        .lock()
        .ok()
        .and_then(|p| {
          p.as_ref()
            .and_then(|p| p.get("deviceScaleFactor"))
            .and_then(serde_json::Value::as_f64)
        })
        .filter(|dsf| *dsf > 0.0);
      let computed = match (metric("contentSize", "width"), metric("cssContentSize", "width")) {
        (Some(device), Some(css)) if css > 0.0 => Some(device / css),
        _ => None,
      };
      clip_scale /= overridden.or(computed).filter(|dsf| *dsf > 0.0).unwrap_or(1.0);
    }
    let format = match capture.format {
      ImageFormat::Png => "png",
      ImageFormat::Jpeg => "jpeg",
      ImageFormat::Webp => "webp",
    };
    let mut params = serde_json::json!({
      "format": format,
      "clip": {
        "x": document_rect.x,
        "y": document_rect.y,
        "width": document_rect.width,
        "height": document_rect.height,
        "scale": clip_scale,
      },
      "captureBeyondViewport": !capture.fits_viewport,
    });
    if let Some(quality) = capture.quality {
      params["quality"] = serde_json::json!(quality);
    }
    // Capturing beyond the viewport resets touch emulation in Chromium
    // (crbug.com/558509412), so a page that had it gets it back.
    let had_touch = !capture.fits_viewport
      && self
        .evaluate("navigator.maxTouchPoints > 0")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let bytes = screenshot::capture(&self.transport, self.session_id.as_deref(), params).await?;
    if had_touch {
      self
        .cmd(
          "Emulation.setTouchEmulationEnabled",
          serde_json::json!({ "enabled": true, "maxTouchPoints": 5 }),
        )
        .await?;
    }
    Ok(bytes)
  }

  // ---- Screencast (video recording) ----

  /// Start CDP screencast. Chrome will emit `Page.screencastFrame` events with JPEG data.
  ///
  /// Returns `(frame_rx, shutdown_tx)`. The caller drives shutdown by
  /// sending `()` on `shutdown_tx` — the listener drains any in-flight
  /// `screencastFrame` events that arrived between
  /// `Page.stopScreencast` and the shutdown signal, then drops the
  /// frame sender so the consumer's `recv()` returns `None`. This is
  /// deterministic: no `sleep` hack to "give frames time to arrive".
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Page.startScreencast`, the target has closed, or the command
  /// outlives its time budget.
  pub async fn start_screencast(
    &self,
    quality: u8,
    max_width: u32,
    max_height: u32,
  ) -> Result<(
    tokio::sync::mpsc::UnboundedReceiver<(Vec<u8>, f64)>,
    tokio::sync::oneshot::Sender<()>,
  )> {
    // Subscribe to the event stream BEFORE firing `Page.startScreencast`
    // so the very first `Page.screencastFrame` event isn't lost in the
    // gap between the command being sent and the listener subscribing.
    // For short recordings (fast goto+close) that single missed frame
    // is the difference between ffmpeg producing a valid file and
    // exiting with "Output file does not contain any stream".
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    Self::spawn_screencast_listener(Arc::clone(&self.transport), self.session_id.clone(), tx, shutdown_rx);

    self
      .cmd(
        "Page.startScreencast",
        serde_json::json!({
          "format": "jpeg",
          "quality": quality,
          "maxWidth": max_width,
          "maxHeight": max_height,
          "everyNthFrame": 1,
        }),
      )
      .await?;

    Ok((rx, shutdown_tx))
  }

  /// Stop CDP screencast.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Page.stopScreencast`, the target has closed, or the command outlives
  /// its time budget.
  pub async fn stop_screencast(&self) -> Result<()> {
    self.cmd("Page.stopScreencast", serde_json::json!({})).await?;
    Ok(())
  }

  /// Background task: listens for `Page.screencastFrame` events, decodes JPEG, acks, forwards.
  ///
  /// Passes raw JPEG frames to the channel. Frame interpolation (gap-filling) is handled
  /// by the video recorder layer, not here. ACK is sent immediately and non-blocking
  /// (matching Playwright's approach) so Chrome sends the next frame ASAP.
  ///
  /// `shutdown_rx` provides cooperative teardown. The select loop is
  /// `biased` so any event already buffered in the broadcast channel
  /// is processed before the shutdown signal is observed — that's
  /// what keeps short recordings from losing the trailing frames
  /// Chrome emitted just before `Page.stopScreencast` landed.
  fn spawn_screencast_listener(
    transport: Arc<CdpTransport>,
    session_id: Option<Arc<str>>,
    frame_tx: tokio::sync::mpsc::UnboundedSender<(Vec<u8>, f64)>,
    shutdown_rx: tokio::sync::oneshot::Receiver<()>,
  ) {
    // Subscribed here, not in the task: frames sent before the task is
    // first polled would otherwise have no receiver.
    let mut rx = transport.subscribe_event_method("Page.screencastFrame");
    tokio::spawn(async move {
      let mut shutdown_rx = shutdown_rx;
      loop {
        let event = tokio::select! {
          biased;
          ev = rx.recv() => match ev {
            Ok(ev) => ev,
            // Screencast frames arrive fast enough to lap the
            // subscription; the lapped frames are lost but exiting
            // here would silently end the whole recording.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
          },
          _ = &mut shutdown_rx => {
            // Drain any events already buffered in the broadcast
            // subscription -- those are the frames Chrome shipped
            // before `Page.stopScreencast` returned. Once the
            // subscription is empty, drop `frame_tx` so the consumer
            // sees end-of-stream.
            loop {
              match rx.try_recv() {
                Ok(ev) => Self::process_screencast_event(&ev, session_id.as_ref(), &transport, &frame_tx),
                // Lagged only reports the overwritten frames — newer
                // ones are still buffered behind it, keep draining.
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {},
                Err(_) => break,
              }
            }
            break;
          },
        };
        Self::process_screencast_event(&event, session_id.as_ref(), &transport, &frame_tx);
      }
    });
  }

  fn process_screencast_event(
    event: &serde_json::Value,
    session_id: Option<&Arc<str>>,
    transport: &Arc<CdpTransport>,
    frame_tx: &tokio::sync::mpsc::UnboundedSender<(Vec<u8>, f64)>,
  ) {
    if let Some(expected_sid) = session_id {
      let event_sid = event.get("sessionId").and_then(|v| v.as_str());
      if event_sid != Some(&**expected_sid) {
        return;
      }
    }
    if event.get("method").and_then(|m| m.as_str()) != Some("Page.screencastFrame") {
      return;
    }
    let Some(params) = event.get("params") else { return };
    let timestamp = params
      .get("metadata")
      .and_then(|m| m.get("timestamp"))
      .and_then(serde_json::Value::as_f64)
      .unwrap_or_else(|| {
        std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .unwrap_or_default()
          .as_secs_f64()
      });
    if let Some(data_str) = params.get("data").and_then(|v| v.as_str())
      && let Ok(jpeg_bytes) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data_str)
    {
      let _ = frame_tx.send((jpeg_bytes, timestamp));
    }
    let ack_id = params.get("sessionId").and_then(serde_json::Value::as_i64).unwrap_or(0);
    let t = Arc::clone(transport);
    let sid = session_id.cloned();
    tokio::spawn(async move {
      let _ = t
        .send_command(
          sid.as_deref(),
          "Page.screencastFrameAck",
          &serde_json::json!({ "sessionId": ack_id }),
        )
        .await;
    });
  }

  // ---- PDF ----

  /// Generate a PDF of the current page using CDP `Page.printToPDF`.
  ///
  /// Param mapping mirrors
  /// `/tmp/playwright/packages/playwright-core/src/server/chromium/crPdf.ts::CRPDF.generate`
  /// 1:1. When `format` is set, its canonical inch dimensions override
  /// `width`/`height`. Otherwise `width`/`height` (if present) are converted
  /// to inches via [`crate::options::PdfSize::to_inches`]. Margins default
  /// to `0` per side and are converted the same way. Every optional field
  /// falls back to Playwright's default when `None`.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Page.printToPDF`, the target has closed, or the command outlives its
  /// time budget. Returns [`FerriError::InvalidArgument`] for an unknown paper `format`, and an error when
  /// the answer carries no data or the data is not valid base64.
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
    let margin_top = margin.top.as_ref().map_or(0.0, crate::options::PdfSize::to_inches);
    let margin_right = margin.right.as_ref().map_or(0.0, crate::options::PdfSize::to_inches);
    let margin_bottom = margin.bottom.as_ref().map_or(0.0, crate::options::PdfSize::to_inches);
    let margin_left = margin.left.as_ref().map_or(0.0, crate::options::PdfSize::to_inches);

    let params = serde_json::json!({
      "landscape": opts.landscape.unwrap_or(false),
      "displayHeaderFooter": opts.display_header_footer.unwrap_or(false),
      "headerTemplate": opts.header_template.unwrap_or_default(),
      "footerTemplate": opts.footer_template.unwrap_or_default(),
      "printBackground": opts.print_background.unwrap_or(false),
      "scale": opts.scale.unwrap_or(1.0),
      "paperWidth": paper_width,
      "paperHeight": paper_height,
      "marginTop": margin_top,
      "marginBottom": margin_bottom,
      "marginLeft": margin_left,
      "marginRight": margin_right,
      "pageRanges": opts.page_ranges.unwrap_or_default(),
      "preferCSSPageSize": opts.prefer_css_page_size.unwrap_or(false),
      "generateTaggedPDF": opts.tagged.unwrap_or(false),
      "generateDocumentOutline": opts.outline.unwrap_or(false),
    });

    let result = self.cmd("Page.printToPDF", params).await?;
    let data = result
      .get("data")
      .and_then(|v| v.as_str())
      .ok_or_else(|| FerriError::backend("No PDF data"))?;
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
      .map_err(|e| FerriError::Backend(format!("Decode PDF: {e}")))
  }

  // ---- File upload ----

  // ---- Accessibility ----

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Accessibility.getFullAXTree`, the target has closed, or the command
  /// outlives its time budget. Also fails when it returns no nodes.
  pub async fn accessibility_tree(&self) -> Result<Vec<AxNodeData>> {
    self.accessibility_tree_with_depth(-1).await
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Accessibility.getFullAXTree`, the target has closed, or the command
  /// outlives its time budget. Also fails when it returns no nodes.
  pub async fn accessibility_tree_with_depth(&self, depth: i32) -> Result<Vec<AxNodeData>> {
    let result = self
      .cmd("Accessibility.getFullAXTree", serde_json::json!({"depth": depth}))
      .await?;

    let nodes = result
      .get("nodes")
      .and_then(|n| n.as_array())
      .ok_or_else(|| FerriError::protocol("Accessibility.getFullAXTree", "No a11y nodes"))?;

    Ok(
      nodes
        .iter()
        .map(|node| {
          let get_ax_value = |field: &str| -> Option<String> {
            node
              .get(field)
              .and_then(|v| v.get("value"))
              .and_then(|v| v.as_str())
              .map(std::string::ToString::to_string)
          };

          let properties = node
            .get("properties")
            .and_then(|p| p.as_array())
            .map(|props| {
              props
                .iter()
                .map(|p| AxProperty {
                  name: p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_lowercase(),
                  value: p.get("value").and_then(|v| v.get("value")).cloned(),
                })
                .collect()
            })
            .unwrap_or_default();

          AxNodeData {
            node_id: node.get("nodeId").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            parent_id: node
              .get("parentId")
              .and_then(|v| v.as_str())
              .map(std::string::ToString::to_string),
            backend_dom_node_id: node.get("backendDOMNodeId").and_then(serde_json::Value::as_i64),
            ignored: node
              .get("ignored")
              .and_then(serde_json::Value::as_bool)
              .unwrap_or(false),
            role: get_ax_value("role"),
            name: get_ax_value("name"),
            description: get_ax_value("description"),
            properties,
          }
        })
        .collect(),
    )
  }
}
