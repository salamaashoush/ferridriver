//! `WebKitPage` screenshots, accessibility, input, tracing, listeners, PDF, and screencast.

use super::*;

impl WebKitPage {
  // ── Screenshots ───────────────────────────────────────────────────────

  /// `wkPage.ts::setBackgroundColor`.
  ///
  /// # Errors
  ///
  /// Returns an error when the `Page.setDefaultBackgroundColorOverride` command fails.
  pub async fn set_background_color(&self, color: Option<BackgroundColor>) -> Result<()> {
    let params = match color {
      Some(c) => json!({ "color": { "r": c.r, "g": c.g, "b": c.b, "a": c.a } }),
      None => json!({}),
    };
    self
      .target_session()
      .send("Page.setDefaultBackgroundColorOverride", params)
      .await
      .map_err(conn_err)?;
    Ok(())
  }

  /// `wkPage.ts::takeScreenshot`.
  ///
  /// # Errors
  ///
  /// Returns `FerriError::Backend` when `capture` has neither a document nor a viewport rect, when
  /// the rect exceeds 32767 device pixels on a non-macOS port, or when the image data is not valid
  /// base64. Also returns an error when the `Page.snapshotRect` command, reading
  /// `devicePixelRatio`, or re-encoding the PNG as WebP fails.
  pub async fn take_screenshot(&self, capture: ScreenshotCapture) -> Result<Vec<u8>> {
    let (rect, coordinate_system) = match (capture.document_rect, capture.viewport_rect) {
      (Some(rect), _) => (rect, "Page"),
      (None, Some(rect)) => (rect, "Viewport"),
      (None, None) => return Err(FerriError::backend("screenshot: no rect to capture")),
    };
    let omit_device_scale_factor = capture.scale == ScreenshotScale::Css;
    self
      .validate_screenshot_dimensions(rect, omit_device_scale_factor)
      .await?;
    // WebKit on macOS has no WebP encoder: capture a PNG and re-encode it.
    let recode_png_to_webp = capture.format == ImageFormat::Webp && cfg!(target_os = "macos");
    let format = match capture.format {
      ImageFormat::Png => "png",
      ImageFormat::Jpeg => "jpeg",
      ImageFormat::Webp if recode_png_to_webp => "png",
      ImageFormat::Webp => "webp",
    };
    let mut params = json!({
      "x": rect.x,
      "y": rect.y,
      "width": rect.width,
      "height": rect.height,
      "coordinateSystem": coordinate_system,
      "omitDeviceScaleFactor": omit_device_scale_factor,
      "format": format,
    });
    if let (Some(quality), false) = (capture.quality, recode_png_to_webp) {
      params["quality"] = json!(quality);
    }
    let resp = self
      .target_session()
      .send("Page.snapshotRect", params)
      .await
      .map_err(conn_err)?;
    let data_url = resp.get("dataURL").and_then(Value::as_str).unwrap_or_default();
    let encoded = data_url.split_once(',').map_or(data_url, |(_, d)| d);
    let bytes = base64::engine::general_purpose::STANDARD
      .decode(encoded)
      .map_err(|e| FerriError::backend(format!("screenshot base64: {e}")))?;
    if recode_png_to_webp {
      return crate::backend::encode_webp_from_png(&bytes, capture.quality);
    }
    Ok(bytes)
  }

  /// Cairo-based ports (Linux, Windows) cannot produce an image wider or
  /// taller than 32767 pixels.
  async fn validate_screenshot_dimensions(
    &self,
    rect: crate::options::ClipRect,
    omit_device_scale_factor: bool,
  ) -> Result<()> {
    if cfg!(target_os = "macos") {
      return Ok(());
    }
    let ratio = if omit_device_scale_factor {
      1.0
    } else {
      self
        .eval_value("window.devicePixelRatio || 1")
        .await?
        .as_f64()
        .unwrap_or(1.0)
    };
    if (rect.width * ratio).ceil() > 32767.0 || (rect.height * ratio).ceil() > 32767.0 {
      return Err(FerriError::backend(
        "Cannot take screenshot larger than 32767 pixels on any dimension",
      ));
    }
    Ok(())
  }

  // ── Accessibility ─────────────────────────────────────────────────────

  /// # Errors
  ///
  /// Returns an error when injecting the selector engine fails, or when evaluating the
  /// accessibility-tree scripts with `Runtime.callFunctionOn` fails or throws.
  pub async fn accessibility_tree(&self) -> Result<Vec<AxNodeData>> {
    self.accessibility_tree_with_depth(-1).await
  }

  /// # Errors
  ///
  /// Returns an error when injecting the selector engine fails, or when evaluating the
  /// accessibility-tree scripts with `Runtime.callFunctionOn` fails or throws.
  pub async fn accessibility_tree_with_depth(&self, max_depth: i32) -> Result<Vec<AxNodeData>> {
    let fd = self.injected_script().await?;
    self.eval_value(crate::selectors::AX_SUPPORT_JS).await?;
    let json_str = self
      .eval_value(&format!("JSON.stringify({fd}.accessibilityTree({max_depth}))"))
      .await?;
    let arr: Vec<Value> = json_str
      .as_str()
      .and_then(|s| serde_json::from_str(s).ok())
      .unwrap_or_default();
    Ok(super::super::events::parse_ax_nodes(&arr))
  }

  // ── Input ─────────────────────────────────────────────────────────────

  /// # Errors
  ///
  /// Returns an error when an `Input.dispatchMouseEvent` command for the move, press or release
  /// fails; the events after it are not sent.
  pub async fn click_at(&self, x: f64, y: f64) -> Result<()> {
    self
      .click_at_with(x, y, &crate::backend::BackendClickArgs::default_left())
      .await
  }

  /// # Errors
  ///
  /// Returns an error when an `Input.dispatchMouseEvent` command for the move, press or release
  /// fails; the events after it are not sent.
  pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<()> {
    let mut args = crate::backend::BackendClickArgs::default_left();
    args.button = crate::options::MouseButton::parse(button).unwrap_or_default();
    args.click_count = click_count;
    self.click_at_with(x, y, &args).await
  }

  /// # Errors
  ///
  /// Returns an error when an `Input.dispatchMouseEvent` command for the move, press or release
  /// fails; the events after it are not sent.
  pub async fn click_at_with(&self, x: f64, y: f64, args: &crate::backend::BackendClickArgs) -> Result<()> {
    super::super::input::click(self, x, y, args).await
  }

  /// # Errors
  ///
  /// Returns an error when the `Input.dispatchMouseEvent` command fails.
  pub async fn hover_at_with(&self, x: f64, y: f64, args: &crate::backend::BackendHoverArgs) -> Result<()> {
    super::super::input::hover(self, x, y, args).await
  }

  /// # Errors
  ///
  /// Returns an error when reading the visual viewport under mobile emulation fails, or when the
  /// `Input.dispatchTapEvent` command fails.
  pub async fn tap_at_with(&self, x: f64, y: f64) -> Result<()> {
    let (x, y) = self.root_view_point(x, y).await?;
    super::super::input::tap(self, x, y).await
  }

  /// `Input.dispatchTapEvent` reaches `WebPage::fakeTouchTap`, which reads
  /// its point in root-view coordinates, while every other input and
  /// every point the injected script computes is in CSS pixels of the
  /// layout viewport. The two agree until mobile emulation scales the
  /// page: a document with no `<meta name=viewport>` lays out 980px wide
  /// and is shrunk to fit, so an unconverted tap lands at the wrong
  /// element (the `<body>` that "intercepts pointer events").
  async fn root_view_point(&self, x: f64, y: f64) -> Result<(f64, f64)> {
    if !self.is_mobile_emulation() {
      return Ok((x, y));
    }
    let viewport = self
      .eval_value(
        "(() => { const v = window.visualViewport; return v ? [v.scale, v.offsetLeft, v.offsetTop] : [1, 0, 0]; })()",
      )
      .await?;
    let component = |i: usize, default: f64| viewport.get(i).and_then(Value::as_f64).unwrap_or(default);
    let (scale, left, top) = (component(0, 1.0), component(1, 0.0), component(2, 0.0));
    Ok(((x - left) * scale, (y - top) * scale))
  }

  /// # Errors
  ///
  /// Returns an error when the `Input.dispatchMouseEvent` command fails.
  pub async fn move_mouse(&self, x: f64, y: f64) -> Result<()> {
    super::super::input::move_mouse(self, x, y).await
  }

  /// # Errors
  ///
  /// Returns an error when an `Input.dispatchMouseEvent` move command fails; the remaining steps
  /// are not sent.
  pub async fn move_mouse_smooth(&self, from_x: f64, from_y: f64, to_x: f64, to_y: f64, steps: u32) -> Result<()> {
    super::super::input::move_mouse_smooth(self, from_x, from_y, to_x, to_y, steps).await
  }

  /// # Errors
  ///
  /// Returns an error when the `Page.updateScrollingState` command, the wait for a compositor frame
  /// through `Runtime.callFunctionOn`, or the `Input.dispatchWheelEvent` command fails.
  pub async fn mouse_wheel(&self, x: f64, y: f64, delta_x: f64, delta_y: f64) -> Result<()> {
    super::super::input::mouse_wheel(self, x, y, delta_x, delta_y).await
  }

  /// # Errors
  ///
  /// Returns an error when the `Input.dispatchMouseEvent` command fails.
  pub async fn mouse_down(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<()> {
    super::super::input::mouse_down(self, x, y, button, click_count).await
  }

  /// # Errors
  ///
  /// Returns an error when the `Input.dispatchMouseEvent` command fails.
  pub async fn mouse_up(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<()> {
    super::super::input::mouse_up(self, x, y, button, click_count).await
  }

  /// # Errors
  ///
  /// Returns an error when the `Page.insertText` command fails.
  pub async fn insert_text(&self, text: &str) -> Result<()> {
    super::super::input::type_text(self, text).await
  }

  pub(crate) async fn dispatch_key_down(&self, _name: &str, stroke: &KeyStroke) -> Result<()> {
    super::super::input::key_down(self, stroke).await
  }

  pub(crate) async fn dispatch_key_up(&self, _name: &str, stroke: &KeyStroke) -> Result<()> {
    super::super::input::key_up(self, stroke).await
  }

  // ── Tracing ───────────────────────────────────────────────────────────

  /// Chrome's `Tracing` domain has no `WebKit` Inspector counterpart, and
  /// every insight computed downstream reads Chrome trace events. These
  /// used to return `Ok`, so a `WebKit` session reported a trace it had
  /// never recorded and the caller got an empty analysis with no error.
  ///
  /// # Errors
  ///
  /// Always returns `FerriError::Unsupported`: the `WebKit` Inspector protocol has no `Tracing`
  /// domain.
  pub async fn start_tracing(&self, _categories: Option<&[String]>) -> Result<()> {
    tokio::task::yield_now().await;
    Err(FerriError::unsupported(
      "Performance tracing is Chromium-only: the WebKit Inspector protocol has no Tracing domain",
    ))
  }

  /// # Errors
  ///
  /// Always returns `FerriError::Unsupported`: the `WebKit` Inspector protocol has no `Tracing`
  /// domain.
  pub async fn stop_tracing(&self) -> Result<Vec<serde_json::Value>> {
    tokio::task::yield_now().await;
    Err(FerriError::unsupported(
      "Performance tracing is Chromium-only: the WebKit Inspector protocol has no Tracing domain",
    ))
  }

  /// # Errors
  ///
  /// Always returns `FerriError::Unsupported`: the `WebKit` Inspector protocol has no
  /// `Performance.getMetrics`.
  pub async fn metrics(&self) -> Result<Vec<MetricData>> {
    tokio::task::yield_now().await;
    Err(FerriError::unsupported(
      "Performance metrics are Chromium-only: the WebKit Inspector protocol has no Performance.getMetrics",
    ))
  }

  // ── Listeners ─────────────────────────────────────────────────────────

  /// Re-route the listener (already spawned in [`Self::attach`]) to
  /// write captured console / network / dialog events into the
  /// caller's [`tokio::sync::RwLock`] sinks. Cheap pointer swap — the
  /// running listener picks up the new sinks on its next event.
  pub fn attach_listeners(
    &self,
    console_log: Arc<tokio::sync::RwLock<Vec<ConsoleMessage>>>,
    network_log: Arc<tokio::sync::RwLock<Vec<NetworkRequest>>>,
    dialog_log: Arc<tokio::sync::RwLock<Vec<DialogEvent>>>,
  ) {
    self.console_log.store(console_log);
    self.network_log.store(network_log);
    self.dialog_log.store(dialog_log);
  }

  // ── PDF / screencast ──────────────────────────────────────────────────

  /// # Errors
  ///
  /// Always returns `FerriError::Unsupported`: the `WebKit` protocol has no `Page.printToPDF`
  /// command.
  pub async fn pdf(&self, _opts: crate::options::PdfOptions) -> Result<Vec<u8>> {
    tokio::task::yield_now().await;
    Err(FerriError::unsupported(
      "PDF generation is not supported on the webkit backend — Playwright's WebKit \
       protocol exposes no `Page.pdf` / `Page.printToPDF` command. Use cdp-pipe or cdp-raw for PDF.",
    ))
  }

  /// # Errors
  ///
  /// Returns an error when the `Screencast.startScreencast` command fails, or
  /// `FerriError::Protocol` when its reply carries no `generation` to acknowledge frames with.
  pub async fn start_screencast(
    &self,
    quality: u8,
    max_width: u32,
    max_height: u32,
  ) -> Result<crate::backend::screencast::RawScreencast> {
    fn frame_of(env: &Envelope) -> Option<(Vec<u8>, f64)> {
      if env.method.as_deref() != Some("Screencast.screencastFrame") {
        return None;
      }
      let data = env.params.get("data").and_then(Value::as_str).unwrap_or("");
      let ts = env.params.get("timestamp").and_then(Value::as_f64).unwrap_or(0.0);
      let bytes = base64::engine::general_purpose::STANDARD.decode(data).ok()?;
      Some((bytes, ts))
    }
    // Subscribe BEFORE startScreencast so we don't drop frames that
    // fire between the call returning and the listener spawning.
    let mut events = self.proxy.events();
    let started = self
      .proxy
      .send(
        "Screencast.startScreencast",
        json!({
          "width": max_width,
          "height": max_height,
          "toolbarHeight": 0,
          "quality": quality,
        }),
      )
      .await
      .map_err(conn_err)?;
    // WebKit holds back further frames until each one is acknowledged
    // with the generation startScreencast returned, as Playwright's
    // wkPage.ts::_onScreencastFrame does.
    let generation = started
      .get("generation")
      .cloned()
      .ok_or_else(|| FerriError::protocol("Screencast.startScreencast", "no generation to acknowledge frames with"))?;
    let acknowledger = self.proxy.clone();
    let acknowledge = move || {
      let proxy = acknowledger.clone();
      let generation = generation.clone();
      tokio::spawn(async move {
        // A frame arriving as the page closes has nobody left to ack it.
        if let Err(error) = proxy
          .send("Screencast.screencastFrameAck", json!({ "generation": generation }))
          .await
        {
          tracing::debug!(target: "ferridriver::webkit", "screencast frame ack: {error}");
        }
      });
    };
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<u8>, f64)>();
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
      loop {
        let env = tokio::select! {
          biased;
          env = events.recv() => match env {
            Some(env) => env,
            None => break,
          },
          _ = &mut shutdown_rx => {
            while let Ok(env) = events.try_recv() {
              if let Some(frame) = frame_of(&env) {
                let _ = tx.send(frame);
              }
            }
            break;
          },
        };
        if let Some(frame) = frame_of(&env) {
          if tx.send(frame).is_err() {
            break;
          }
          acknowledge();
        }
      }
    });
    Ok((rx, shutdown_tx))
  }

  /// # Errors
  ///
  /// Never fails; a failed `Screencast.stopScreencast` is ignored and the `Result` matches the
  /// other backends' signature.
  pub async fn stop_screencast(&self) -> Result<()> {
    let _ = self.proxy.send("Screencast.stopScreencast", json!({})).await;
    Ok(())
  }
}
