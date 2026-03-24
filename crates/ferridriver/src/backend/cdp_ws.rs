//! CdpWs backend — wraps chromiumoxide (Chrome DevTools Protocol over WebSocket).
//! This is the original/default backend.

use super::*;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::accessibility::AxNode;
use chromiumoxide::cdp::browser_protocol::dom::{BackendNodeId, ResolveNodeParams};
use chromiumoxide::cdp::browser_protocol::network::{
    EventRequestWillBeSent, EventResponseReceived,
};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::cdp::js_protocol::runtime::{CallFunctionOnParams, EventConsoleApiCalled};
use chromiumoxide::Page;
use base64::Engine as _;
use futures::StreamExt;

// ─── CdpWsBrowser ───────────────────────────────────────────────────────────

pub struct CdpWsBrowser {
    browser: Browser,
    _handler_handle: tokio::task::JoinHandle<()>,
}

impl CdpWsBrowser {
    pub async fn launch(chromium_path: &str) -> Result<Self, String> {
        let user_data_dir =
            std::env::temp_dir().join(format!("ferridriver-{}", std::process::id()));

        let mut builder = BrowserConfig::builder()
            .chrome_executable(chromium_path)
            .user_data_dir(user_data_dir)
            .viewport(None);

        // Apply shared Chrome flags (skipping --headless and --no-sandbox
        // which BrowserConfig handles via its own methods)
        for flag in crate::state::CHROME_FLAGS {
            match *flag {
                "--headless" => {} // BrowserConfig handles this
                "--no-sandbox" => { builder = builder.no_sandbox(); }
                f => { builder = builder.arg(f); }
            }
        }

        let config = builder
            .build()
            .map_err(|e| format!("Browser config error: {e}"))?;

        let (browser, mut handler) = Browser::launch(config)
            .await
            .map_err(|e| format!("Browser launch failed: {e}"))?;

        let handle = tokio::spawn(async move {
            while let Some(h) = handler.next().await {
                if h.is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            browser,
            _handler_handle: handle,
        })
    }

    pub async fn connect(ws_url: &str) -> Result<Self, String> {
        let (browser, mut handler) = Browser::connect(ws_url)
            .await
            .map_err(|e| format!("Connect failed: {e}"))?;

        let handle = tokio::spawn(async move {
            while let Some(h) = handler.next().await {
                if h.is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            browser,
            _handler_handle: handle,
        })
    }

    pub async fn pages(&self) -> Result<Vec<AnyPage>, String> {
        let pages = self
            .browser
            .pages()
            .await
            .map_err(|e| format!("List pages: {e}"))?;
        for p in &pages {
            Self::inject_engine(p).await;
        }
        Ok(pages.into_iter().map(|p| AnyPage::CdpWs(CdpWsPage(p))).collect())
    }

    pub async fn new_page(&self, url: &str) -> Result<AnyPage, String> {
        let page = self
            .browser
            .new_page(url)
            .await
            .map_err(|e| format!("New page failed: {e}"))?;
        Self::inject_engine(&page).await;
        Ok(AnyPage::CdpWs(CdpWsPage(page)))
    }

    /// Inject selector engine via addScriptToEvaluateOnNewDocument so it's
    /// available on every navigation without a separate evaluate call.
    async fn inject_engine(page: &Page) {
        use chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams;
        let engine_js = crate::selectors::build_inject_js();
        let _ = page.execute(AddScriptToEvaluateOnNewDocumentParams::new(engine_js)).await;
    }

    pub async fn new_page_isolated(&self, url: &str) -> Result<AnyPage, String> {
        use chromiumoxide::cdp::browser_protocol::target::{
            CreateBrowserContextParams, CreateTargetParams,
        };
        let ctx_id = {
            let params = CreateBrowserContextParams::builder().build();
            let result = self
                .browser
                .execute(params)
                .await
                .map_err(|e| format!("Create browser context failed: {e}"))?;
            result.result.browser_context_id
        };
        let mut create_params = CreateTargetParams::new(url);
        create_params.browser_context_id = Some(ctx_id);
        let page = self
            .browser
            .new_page(create_params)
            .await
            .map_err(|e| format!("New page in context failed: {e}"))?;
        Ok(AnyPage::CdpWs(CdpWsPage(page)))
    }

    pub async fn close(&mut self) -> Result<(), String> {
        self.browser.close().await.map_err(|e| format!("Close: {e}")).map(|_| ())
    }
}

// ─── CdpWsPage ──────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct CdpWsPage(pub(crate) Page);

impl CdpWsPage {
    // ── Navigation ──

    pub async fn goto(&self, url: &str) -> Result<(), String> {
        self.0.goto(url).await.map_err(|e| format!("Navigate: {e}"))?;
        Ok(())
    }

    pub async fn wait_for_navigation(&self) -> Result<(), String> {
        let _ = self.0.wait_for_navigation().await;
        Ok(())
    }

    pub async fn reload(&self) -> Result<(), String> {
        self.0.reload().await.map_err(|e| format!("Reload: {e}"))?;
        Ok(())
    }

    pub async fn go_back(&self) -> Result<(), String> {
        self.0.evaluate("window.history.back()").await.map_err(|e| format!("{e}"))?;
        let _ = self.0.wait_for_navigation().await;
        Ok(())
    }

    pub async fn go_forward(&self) -> Result<(), String> {
        self.0.evaluate("window.history.forward()").await.map_err(|e| format!("{e}"))?;
        let _ = self.0.wait_for_navigation().await;
        Ok(())
    }

    pub async fn url(&self) -> Result<Option<String>, String> {
        self.0.url().await.map_err(|e| format!("URL: {e}"))
    }

    pub async fn title(&self) -> Result<Option<String>, String> {
        self.0.get_title().await.map_err(|e| format!("Title: {e}"))
    }

    // ── JavaScript ──

    pub async fn evaluate(&self, expression: &str) -> Result<Option<serde_json::Value>, String> {
        let result = self.0.evaluate(expression).await.map_err(|e| format!("{e}"))?;
        Ok(result.value().cloned())
    }

    // ── Elements ──

    pub async fn find_element(&self, selector: &str) -> Result<AnyElement, String> {
        let el = self
            .0
            .find_element(selector)
            .await
            .map_err(|e| format!("'{selector}': {e}"))?;
        Ok(AnyElement::CdpWs(CdpWsElement { element: el, page: self.0.clone() }))
    }

    pub async fn evaluate_to_element(&self, js: &str) -> Result<AnyElement, String> {
        use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
        use chromiumoxide::cdp::browser_protocol::dom::RequestNodeParams;

        // Evaluate without returnByValue to get a RemoteObject reference
        let params = EvaluateParams::builder()
            .expression(js)
            .build()
            .map_err(|e| format!("{e}"))?;
        let result = self.0.execute(params).await.map_err(|e| format!("{e}"))?;
        let object_id = result.result.result.object_id
            .ok_or("JS did not return a DOM element")?;

        // Get nodeId from objectId
        let node_result = self.0.execute(RequestNodeParams::new(object_id))
            .await
            .map_err(|e| format!("{e}"))?;
        let node_id = node_result.result.node_id;

        // Now find the element via chromiumoxide using the nodeId
        // We need to get a chromiumoxide Element. Use DOM.describeNode to get the backendNodeId,
        // then resolve via find_element with a unique attribute.
        // Simpler approach: tag the element in JS with a unique attr, then find_element by it.
        let tag = format!("fd-eval-{}", std::process::id());
        let tag_js = format!(
            "(function() {{ var el = ({js}); if (el) el.setAttribute('data-{tag}', '1'); }})()"
        );
        let _ = self.0.evaluate(tag_js).await;
        let el = self.0.find_element(format!("[data-{tag}]"))
            .await
            .map_err(|e| format!("{e}"))?;
        let _ = self.0.evaluate(format!(
            "document.querySelector('[data-{tag}]')?.removeAttribute('data-{tag}')"
        )).await;
        Ok(AnyElement::CdpWs(CdpWsElement { element: el, page: self.0.clone() }))
    }

    // ── Content ──

    pub async fn content(&self) -> Result<String, String> {
        self.0.content().await.map_err(|e| format!("{e}"))
    }

    pub async fn set_content(&self, html: &str) -> Result<(), String> {
        let frame_id = self
            .0
            .mainframe()
            .await
            .map_err(|e| format!("No frame: {e}"))?
            .ok_or_else(|| "No main frame".to_string())?;
        let engine_js = crate::selectors::build_inject_js();
        let augmented = format!("<script>{engine_js}</script>{html}");
        self.0
            .execute(
                chromiumoxide::cdp::browser_protocol::page::SetDocumentContentParams::new(
                    frame_id,
                    augmented,
                ),
            )
            .await
            .map_err(|e| format!("{e}"))
            .map(|_| ())
    }

    // ── Screenshots ──

    pub async fn screenshot(&self, opts: ScreenshotOpts) -> Result<Vec<u8>, String> {
        use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotParams;

        // Use raw CDP Page.captureScreenshot directly instead of chromiumoxide's
        // screenshot wrapper which adds significant overhead (537ms vs 25ms).
        let format = match opts.format {
            ImageFormat::Png => CaptureScreenshotFormat::Png,
            ImageFormat::Jpeg => CaptureScreenshotFormat::Jpeg,
            ImageFormat::Webp => CaptureScreenshotFormat::Webp,
        };
        let mut params = CaptureScreenshotParams::builder()
            .format(format)
            .optimize_for_speed(true);
        if let Some(q) = opts.quality {
            params = params.quality(q);
        }

        if opts.full_page {
            use chromiumoxide::cdp::browser_protocol::page::GetLayoutMetricsParams;
            let metrics = self.0.execute(GetLayoutMetricsParams::default())
                .await
                .map_err(|e| format!("Layout metrics: {e}"))?;
            let cs = &metrics.result.css_content_size;
            use chromiumoxide::cdp::browser_protocol::page::Viewport;
            params = params.clip(Viewport {
                x: 0.0,
                y: 0.0,
                width: cs.width,
                height: cs.height,
                scale: 1.0,
            });
        }

        let result = self.0.execute(params.build())
            .await
            .map_err(|e| format!("Screenshot: {e}"))?;

        base64::engine::general_purpose::STANDARD
            .decode(&result.result.data)
            .map_err(|e| format!("Decode: {e}"))
    }

    pub async fn screenshot_element(
        &self,
        selector: &str,
        format: ImageFormat,
    ) -> Result<Vec<u8>, String> {
        let el = self
            .0
            .find_element(selector)
            .await
            .map_err(|e| format!("{e}"))?;
        let fmt = match format {
            ImageFormat::Png => CaptureScreenshotFormat::Png,
            ImageFormat::Jpeg => CaptureScreenshotFormat::Jpeg,
            ImageFormat::Webp => CaptureScreenshotFormat::Webp,
        };
        el.screenshot(fmt).await.map_err(|e| format!("{e}"))
    }

    // ── PDF ──

    pub async fn pdf(&self, landscape: bool, print_background: bool) -> Result<Vec<u8>, String> {
        use chromiumoxide::cdp::browser_protocol::page::PrintToPdfParams;
        let mut params = PrintToPdfParams::default();
        params.landscape = Some(landscape);
        params.print_background = Some(print_background);
        params.prefer_css_page_size = Some(true);
        let result = self.0.execute(params).await.map_err(|e| format!("PDF: {e}"))?;
        let data = result.result.data;
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &data)
            .map_err(|e| format!("Decode PDF: {e}"))
    }

    // ── File upload ──

    pub async fn set_file_input(&self, selector: &str, paths: &[String]) -> Result<(), String> {
        use chromiumoxide::cdp::browser_protocol::dom::{
            GetDocumentParams, QuerySelectorParams, DescribeNodeParams, SetFileInputFilesParams,
            BackendNodeId,
        };
        // Get document root
        let doc = self.0.execute(GetDocumentParams::default()).await
            .map_err(|e| format!("Get document: {e}"))?;
        let root_node_id = doc.result.root.node_id;

        // Query for the element
        let query = QuerySelectorParams::new(root_node_id, selector.to_string());
        let query_result = self.0.execute(query).await
            .map_err(|e| format!("querySelector '{selector}': {e}"))?;
        let node_id = query_result.result.node_id;

        // Get backendNodeId via describeNode
        let describe = DescribeNodeParams::builder().node_id(node_id).build();
        let desc_result = self.0.execute(describe).await
            .map_err(|e| format!("describeNode: {e}"))?;
        let backend_node_id = desc_result.result.node.backend_node_id;

        // Set files
        let mut params = SetFileInputFilesParams::new(paths.to_vec());
        params.backend_node_id = Some(BackendNodeId::new(backend_node_id.inner().clone()));
        self.0.execute(params).await
            .map_err(|e| format!("setFileInputFiles: {e}"))?;
        Ok(())
    }

    // ── Accessibility ──

    pub async fn accessibility_tree(&self) -> Result<Vec<AxNodeData>, String> {
        let tree = self
            .0
            .get_full_ax_tree(Some(-1), None)
            .await
            .map_err(|e| format!("A11y tree: {e}"))?;
        Ok(convert_ax_nodes(&tree.nodes))
    }

    // ── Input ──

    pub async fn click_at(&self, x: f64, y: f64) -> Result<(), String> {
        self.click_at_opts(x, y, "left", 1).await
    }

    pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<(), String> {
        use chromiumoxide::cdp::browser_protocol::input::{DispatchMouseEventParams, DispatchMouseEventType, MouseButton};
        let btn = match button {
            "right" => MouseButton::Right,
            "middle" => MouseButton::Middle,
            "back" => MouseButton::Back,
            "forward" => MouseButton::Forward,
            _ => MouseButton::Left,
        };
        let pressed = DispatchMouseEventParams::builder()
            .r#type(DispatchMouseEventType::MousePressed)
            .x(x).y(y).button(btn.clone()).click_count(click_count as i64)
            .build().map_err(|e| format!("{e}"))?;
        self.0.execute(pressed).await.map_err(|e| format!("{e}"))?;
        let released = DispatchMouseEventParams::builder()
            .r#type(DispatchMouseEventType::MouseReleased)
            .x(x).y(y).button(btn).click_count(click_count as i64)
            .build().map_err(|e| format!("{e}"))?;
        self.0.execute(released).await.map_err(|e| format!("{e}"))?;
        Ok(())
    }

    pub async fn move_mouse(&self, x: f64, y: f64) -> Result<(), String> {
        use chromiumoxide::cdp::browser_protocol::input::{DispatchMouseEventParams, DispatchMouseEventType};
        let moved = DispatchMouseEventParams::builder()
            .r#type(DispatchMouseEventType::MouseMoved)
            .x(x).y(y)
            .build().map_err(|e| format!("{e}"))?;
        self.0.execute(moved).await.map_err(|e| format!("{e}"))?;
        Ok(())
    }

    pub async fn move_mouse_smooth(&self, from_x: f64, from_y: f64, to_x: f64, to_y: f64, steps: u32) -> Result<(), String> {
        let steps = steps.max(1);
        for i in 0..=steps {
            let t = i as f64 / steps as f64;
            let ease = t * t * (3.0 - 2.0 * t);
            let x = from_x + (to_x - from_x) * ease;
            let y = from_y + (to_y - from_y) * ease;
            self.move_mouse(x, y).await?;
        }
        Ok(())
    }

    pub async fn click_and_drag(&self, from: (f64, f64), to: (f64, f64)) -> Result<(), String> {
        use chromiumoxide::cdp::browser_protocol::input::{DispatchMouseEventParams, DispatchMouseEventType, MouseButton};
        let pressed = DispatchMouseEventParams::builder()
            .r#type(DispatchMouseEventType::MousePressed)
            .x(from.0).y(from.1).button(MouseButton::Left).click_count(1i64)
            .build().map_err(|e| format!("{e}"))?;
        self.0.execute(pressed).await.map_err(|e| format!("{e}"))?;
        let steps = 10u32;
        for i in 1..=steps {
            let t = i as f64 / steps as f64;
            let ease = t * t * (3.0 - 2.0 * t);
            let x = from.0 + (to.0 - from.0) * ease;
            let y = from.1 + (to.1 - from.1) * ease;
            self.move_mouse(x, y).await?;
        }
        let released = DispatchMouseEventParams::builder()
            .r#type(DispatchMouseEventType::MouseReleased)
            .x(to.0).y(to.1).button(MouseButton::Left).click_count(1i64)
            .build().map_err(|e| format!("{e}"))?;
        self.0.execute(released).await.map_err(|e| format!("{e}"))?;
        Ok(())
    }

    pub async fn type_str(&self, text: &str) -> Result<(), String> {
        self.0.type_str(text).await.map_err(|e| format!("{e}")).map(|_| ())
    }

    pub async fn press_key(&self, key: &str) -> Result<(), String> {
        self.0.press_key(key).await.map_err(|e| format!("{e}")).map(|_| ())
    }

    // ── Cookies ──

    pub async fn get_cookies(&self) -> Result<Vec<CookieData>, String> {
        let cookies = self.0.get_cookies().await.map_err(|e| format!("{e}"))?;
        Ok(cookies
            .iter()
            .map(|c| CookieData {
                name: c.name.clone(),
                value: c.value.clone(),
                domain: c.domain.clone(),
                path: c.path.clone(),
                secure: c.secure,
                http_only: c.http_only,
                expires: Some(c.expires),
            })
            .collect())
    }

    pub async fn set_cookie(&self, cookie: CookieData) -> Result<(), String> {
        use chromiumoxide::cdp::browser_protocol::network::{CookieParam, TimeSinceEpoch};
        let mut cp = CookieParam::new(cookie.name, cookie.value);
        if !cookie.domain.is_empty() {
            cp.domain = Some(cookie.domain);
        }
        if !cookie.path.is_empty() {
            cp.path = Some(cookie.path);
        }
        cp.secure = Some(cookie.secure);
        cp.http_only = Some(cookie.http_only);
        if let Some(e) = cookie.expires {
            cp.expires = Some(TimeSinceEpoch::new(e));
        }
        self.0.set_cookie(cp).await.map_err(|e| format!("{e}")).map(|_| ())
    }

    pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<(), String> {
        use chromiumoxide::cdp::browser_protocol::network::DeleteCookiesParams;
        let mut params = DeleteCookiesParams::new(name.to_string());
        params.domain = domain.map(|d| d.to_string());
        self.0.delete_cookie(params).await.map_err(|e| format!("{e}")).map(|_| ())
    }

    pub async fn clear_cookies(&self) -> Result<(), String> {
        self.0.clear_cookies().await.map_err(|e| format!("{e}")).map(|_| ())
    }

    // ── Emulation ──

    pub async fn emulate_viewport(&self, config: &crate::options::ViewportConfig) -> Result<(), String> {
        use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
        let mut params = SetDeviceMetricsOverrideParams::new(
            config.width, config.height, config.device_scale_factor, config.is_mobile,
        );
        // SetDeviceMetricsOverrideParams doesn't expose screenWidth/screenHeight/touch directly
        // via the builder, but the struct fields are public
        self.0
            .emulate_viewport(params)
            .await
            .map_err(|e| format!("{e}"))
            .map(|_| ())
    }

    pub async fn set_user_agent(&self, ua: &str) -> Result<(), String> {
        use chromiumoxide::cdp::browser_protocol::network::SetUserAgentOverrideParams;
        self.0
            .set_user_agent(SetUserAgentOverrideParams::new(ua.to_string()))
            .await
            .map_err(|e| format!("{e}"))
            .map(|_| ())
    }

    pub async fn set_geolocation(
        &self,
        lat: f64,
        lng: f64,
        accuracy: f64,
    ) -> Result<(), String> {
        use chromiumoxide::cdp::browser_protocol::emulation::SetGeolocationOverrideParams;
        let params = SetGeolocationOverrideParams::builder()
            .latitude(lat)
            .longitude(lng)
            .accuracy(accuracy)
            .build();
        self.0
            .emulate_geolocation(params)
            .await
            .map_err(|e| format!("{e}"))
            .map(|_| ())
    }

    pub async fn set_locale(&self, locale: &str) -> Result<(), String> {
        use chromiumoxide::cdp::browser_protocol::emulation::SetLocaleOverrideParams;
        let _ = self.0.execute(SetLocaleOverrideParams::builder().locale(locale).build()).await;
        // Also set via Network.setUserAgentOverride for navigator.language
        use chromiumoxide::cdp::browser_protocol::network::SetUserAgentOverrideParams;
        let mut params = SetUserAgentOverrideParams::new("");
        params.accept_language = Some(locale.to_string());
        self.0.execute(params).await.map_err(|e| format!("{e}")).map(|_| ())
    }

    pub async fn set_timezone(&self, timezone_id: &str) -> Result<(), String> {
        use chromiumoxide::cdp::browser_protocol::emulation::SetTimezoneOverrideParams;
        self.0.execute(SetTimezoneOverrideParams::new(timezone_id))
            .await.map_err(|e| format!("{e}")).map(|_| ())
    }

    pub async fn emulate_media(&self, opts: &crate::options::EmulateMediaOptions) -> Result<(), String> {
        use chromiumoxide::cdp::browser_protocol::emulation::{SetEmulatedMediaParams, MediaFeature};
        let mut features = Vec::new();
        if let Some(cs) = &opts.color_scheme {
            features.push(MediaFeature { name: "prefers-color-scheme".into(), value: cs.clone() });
        }
        if let Some(rm) = &opts.reduced_motion {
            features.push(MediaFeature { name: "prefers-reduced-motion".into(), value: rm.clone() });
        }
        if let Some(fc) = &opts.forced_colors {
            features.push(MediaFeature { name: "forced-colors".into(), value: fc.clone() });
        }
        if let Some(c) = &opts.contrast {
            features.push(MediaFeature { name: "prefers-contrast".into(), value: c.clone() });
        }
        let mut params = SetEmulatedMediaParams::default();
        params.media = opts.media.clone();
        params.features = Some(features);
        self.0.execute(params).await.map_err(|e| format!("{e}")).map(|_| ())
    }

    pub async fn set_javascript_enabled(&self, enabled: bool) -> Result<(), String> {
        use chromiumoxide::cdp::browser_protocol::emulation::SetScriptExecutionDisabledParams;
        self.0.execute(SetScriptExecutionDisabledParams::new(!enabled))
            .await.map_err(|e| format!("{e}")).map(|_| ())
    }

    pub async fn set_extra_http_headers(&self, headers: &rustc_hash::FxHashMap<String, String>) -> Result<(), String> {
        use chromiumoxide::cdp::browser_protocol::network::SetExtraHttpHeadersParams;
        use chromiumoxide::cdp::browser_protocol::network::Headers;
        let h: serde_json::Map<String, serde_json::Value> = headers.iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect();
        let params = SetExtraHttpHeadersParams::new(Headers::new(serde_json::Value::Object(h)));
        self.0.execute(params).await.map_err(|e| format!("{e}")).map(|_| ())
    }

    pub async fn grant_permissions(&self, _permissions: &[String], _origin: Option<&str>) -> Result<(), String> {
        // Browser-level command, not page-level. Would need browser handle.
        Ok(())
    }

    pub async fn reset_permissions(&self) -> Result<(), String> {
        Ok(())
    }

    pub async fn set_focus_emulation_enabled(&self, enabled: bool) -> Result<(), String> {
        use chromiumoxide::cdp::browser_protocol::emulation::SetFocusEmulationEnabledParams;
        self.0.execute(SetFocusEmulationEnabledParams::new(enabled))
            .await.map_err(|e| format!("{e}")).map(|_| ())
    }

    // ── Network ──

    pub async fn set_network_state(
        &self,
        offline: bool,
        latency: f64,
        download: f64,
        upload: f64,
    ) -> Result<(), String> {
        use chromiumoxide::cdp::browser_protocol::network::OverrideNetworkStateParams;
        let params = OverrideNetworkStateParams::new(offline, latency, download, upload);
        self.0.execute(params).await.map_err(|e| format!("{e}"))?;
        Ok(())
    }

    // ── Tracing ──

    pub async fn start_tracing(&self) -> Result<(), String> {
        let params = chromiumoxide::cdp::browser_protocol::tracing::StartParams::builder().build();
        self.0.execute(params).await.map_err(|e| format!("{e}"))?;
        Ok(())
    }

    pub async fn stop_tracing(&self) -> Result<(), String> {
        self.0
            .execute(chromiumoxide::cdp::browser_protocol::tracing::EndParams {})
            .await
            .map_err(|e| format!("{e}"))?;
        Ok(())
    }

    pub async fn metrics(&self) -> Result<Vec<MetricData>, String> {
        let metrics = self.0.metrics().await.map_err(|e| format!("{e}"))?;
        Ok(metrics
            .iter()
            .map(|m| MetricData {
                name: m.name.clone(),
                value: m.value,
            })
            .collect())
    }

    // ── Ref resolution ──

    pub async fn resolve_backend_node(
        &self,
        backend_node_id: i64,
        ref_id: &str,
    ) -> Result<AnyElement, String> {
        let resolve = ResolveNodeParams::builder()
            .backend_node_id(BackendNodeId::new(backend_node_id))
            .build();
        let resolved = self
            .0
            .execute(resolve)
            .await
            .map_err(|e| format!("Ref '{ref_id}' stale: {e}"))?;
        let oid = resolved
            .result
            .object
            .object_id
            .ok_or_else(|| format!("Ref '{ref_id}' no longer valid."))?;

        let tag = CallFunctionOnParams::builder()
            .object_id(oid)
            .function_declaration(format!(
                "function() {{ this.setAttribute('data-cref', '{ref_id}'); }}"
            ))
            .build()
            .map_err(|e| format!("Tag build error: {e}"))?;
        self.0
            .execute(tag)
            .await
            .map_err(|e| format!("Tag failed: {e}"))?;

        let el = self
            .0
            .find_element(&format!("[data-cref='{ref_id}']"))
            .await
            .map_err(|e| format!("Ref '{ref_id}' element not found: {e}"))?;

        Ok(AnyElement::CdpWs(CdpWsElement { element: el, page: self.0.clone() }))
    }

    // ── Event listeners ──

    pub fn attach_listeners(
        &self,
        console_log: Arc<RwLock<Vec<ConsoleMsg>>>,
        network_log: Arc<RwLock<Vec<NetRequest>>>,
        dialog_log: Arc<RwLock<Vec<crate::state::DialogEvent>>>,
    ) {
        // Console listener
        let cl = console_log;
        let page_clone = self.0.clone();
        tokio::spawn(async move {
            if let Ok(mut stream) = page_clone.event_listener::<EventConsoleApiCalled>().await {
                while let Some(ev) = stream.next().await {
                    let text = ev
                        .args
                        .iter()
                        .filter_map(|a| {
                            a.value
                                .as_ref()
                                .map(|v| v.to_string().trim_matches('"').to_string())
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    cl.write().await.push(ConsoleMsg {
                        level: format!("{:?}", ev.r#type).to_lowercase(),
                        text,
                    });
                }
            }
        });

        // Network request listener
        let nl = network_log.clone();
        let page_clone = self.0.clone();
        tokio::spawn(async move {
            if let Ok(mut stream) = page_clone.event_listener::<EventRequestWillBeSent>().await {
                while let Some(ev) = stream.next().await {
                    nl.write().await.push(NetRequest {
                        id: ev.request_id.inner().clone(),
                        method: ev.request.method.clone(),
                        url: ev.request.url.clone(),
                        resource_type: ev
                            .r#type
                            .as_ref()
                            .map(|t| format!("{t:?}"))
                            .unwrap_or_default(),
                        status: None,
                        mime_type: None,
                    });
                }
            }
        });

        // Network response listener (updates status)
        let nl2 = network_log;
        let page_clone = self.0.clone();
        tokio::spawn(async move {
            if let Ok(mut stream) = page_clone.event_listener::<EventResponseReceived>().await {
                while let Some(ev) = stream.next().await {
                    let rid = ev.request_id.inner().clone();
                    let mut reqs = nl2.write().await;
                    if let Some(req) = reqs.iter_mut().rev().find(|r| r.id == rid) {
                        req.status = Some(ev.response.status);
                        req.mime_type = Some(ev.response.mime_type.clone());
                    }
                }
            }
        });

        // Dialog auto-dismiss listener
        use chromiumoxide::cdp::browser_protocol::page::{
            EventJavascriptDialogOpening, HandleJavaScriptDialogParams, DialogType,
        };
        let dl = dialog_log;
        let page_clone = self.0.clone();
        tokio::spawn(async move {
            if let Ok(mut stream) = page_clone.event_listener::<EventJavascriptDialogOpening>().await {
                while let Some(ev) = stream.next().await {
                    let accept = !matches!(ev.r#type, DialogType::Prompt);
                    let _ = page_clone.execute(HandleJavaScriptDialogParams::new(accept)).await;
                    dl.write().await.push(crate::state::DialogEvent {
                        dialog_type: format!("{:?}", ev.r#type).to_lowercase(),
                        message: ev.message.clone(),
                        action: if accept { "accepted".into() } else { "dismissed".into() },
                    });
                }
            }
        });
    }
}

// ─── CdpWsElement ───────────────────────────────────────────────────────────

pub struct CdpWsElement {
    element: chromiumoxide::Element,
    page: Page,
}

impl CdpWsElement {
    pub async fn click(&self) -> Result<(), String> {
        // Single JS call: scroll into view + get center. Same pattern as cdp-pipe/cdp-raw.
        let center = self.call_js_fn_value(
            "function() { this.scrollIntoViewIfNeeded(); var r = this.getBoundingClientRect(); return {x: r.x + r.width/2, y: r.y + r.height/2}; }"
        ).await?;

        if let Some(c) = center {
            let x = c.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = c.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            if x == 0.0 && y == 0.0 {
                let _ = self.element.call_js_fn("function() { this.click(); }", false).await;
                return Ok(());
            }
            use chromiumoxide::cdp::browser_protocol::input::DispatchMouseEventParams;
            let pressed = DispatchMouseEventParams::builder()
                .r#type(chromiumoxide::cdp::browser_protocol::input::DispatchMouseEventType::MousePressed)
                .x(x).y(y).button(chromiumoxide::cdp::browser_protocol::input::MouseButton::Left).click_count(1)
                .build().map_err(|e| format!("{e}"))?;
            self.page.execute(pressed).await.map_err(|e| format!("{e}"))?;
            let released = DispatchMouseEventParams::builder()
                .r#type(chromiumoxide::cdp::browser_protocol::input::DispatchMouseEventType::MouseReleased)
                .x(x).y(y).button(chromiumoxide::cdp::browser_protocol::input::MouseButton::Left).click_count(1)
                .build().map_err(|e| format!("{e}"))?;
            self.page.execute(released).await.map_err(|e| format!("{e}"))?;
            Ok(())
        } else {
            let _ = self.element.call_js_fn("function() { this.click(); }", false).await;
            Ok(())
        }
    }

    pub async fn hover(&self) -> Result<(), String> {
        let _ = self.element.scroll_into_view().await;
        self.element.hover().await.map_err(|e| format!("{e}")).map(|_| ())
    }

    pub async fn type_str(&self, text: &str) -> Result<(), String> {
        self.element.type_str(text).await.map_err(|e| format!("{e}")).map(|_| ())
    }

    pub async fn call_js_fn(&self, function: &str) -> Result<(), String> {
        let _ = self.element.call_js_fn(function, false).await;
        Ok(())
    }

    pub async fn call_js_fn_value(&self, function: &str) -> Result<Option<serde_json::Value>, String> {
        // Use raw CDP CallFunctionOn with returnByValue: true so objects
        // (like DOMRect from getBoundingClientRect) are serialized as JSON.
        let params = CallFunctionOnParams::builder()
            .object_id(self.element.remote_object_id.clone())
            .function_declaration(function)
            .return_by_value(true)
            .build()
            .map_err(|e| format!("{e}"))?;
        let result = self.page.execute(params)
            .await
            .map_err(|e| format!("{e}"))?;
        Ok(result.result.result.value.clone())
    }

    pub async fn scroll_into_view(&self) -> Result<(), String> {
        self.element.scroll_into_view().await.map_err(|e| format!("{e}")).map(|_| ())
    }

    pub async fn screenshot(&self, format: ImageFormat) -> Result<Vec<u8>, String> {
        let fmt = match format {
            ImageFormat::Png => CaptureScreenshotFormat::Png,
            ImageFormat::Jpeg => CaptureScreenshotFormat::Jpeg,
            ImageFormat::Webp => CaptureScreenshotFormat::Webp,
        };
        self.element.screenshot(fmt).await.map_err(|e| format!("{e}"))
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Convert chromiumoxide AxNode to backend-agnostic AxNodeData.
fn convert_ax_nodes(nodes: &[AxNode]) -> Vec<AxNodeData> {
    nodes
        .iter()
        .map(|node| {
            let role = node
                .role
                .as_ref()
                .and_then(|v| v.value.as_ref())
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let name = node
                .name
                .as_ref()
                .and_then(|v| v.value.as_ref())
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let description = node
                .description
                .as_ref()
                .and_then(|v| v.value.as_ref())
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let properties = node
                .properties
                .as_ref()
                .map(|props| {
                    props
                        .iter()
                        .map(|p| AxProperty {
                            name: format!("{:?}", p.name).to_lowercase(),
                            value: p.value.value.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default();

            AxNodeData {
                node_id: node.node_id.inner().clone(),
                parent_id: node.parent_id.as_ref().map(|id| id.inner().clone()),
                backend_dom_node_id: node.backend_dom_node_id.map(|id| *id.inner()),
                ignored: node.ignored,
                role,
                name,
                description,
                properties,
            }
        })
        .collect()
}
