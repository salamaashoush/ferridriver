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
        Ok(pages.into_iter().map(|p| AnyPage::CdpWs(CdpWsPage(p))).collect())
    }

    pub async fn new_page(&self, url: &str) -> Result<AnyPage, String> {
        let page = self
            .browser
            .new_page(url)
            .await
            .map_err(|e| format!("New page failed: {e}"))?;
        Ok(AnyPage::CdpWs(CdpWsPage(page)))
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
        self.0
            .execute(
                chromiumoxide::cdp::browser_protocol::page::SetDocumentContentParams::new(
                    frame_id,
                    html.to_string(),
                ),
            )
            .await
            .map_err(|e| format!("set_content: {e}"))?;
        Ok(())
    }

    // ── Screenshots ──

    pub async fn screenshot(&self, opts: ScreenshotOpts) -> Result<Vec<u8>, String> {
        let format = match opts.format {
            ImageFormat::Png => CaptureScreenshotFormat::Png,
            ImageFormat::Jpeg => CaptureScreenshotFormat::Jpeg,
            ImageFormat::Webp => CaptureScreenshotFormat::Webp,
        };
        let mut builder = chromiumoxide::page::ScreenshotParams::builder().format(format);
        if let Some(q) = opts.quality {
            builder = builder.quality(q);
        }
        if opts.full_page {
            builder = builder.full_page(true);
        }
        self.0
            .screenshot(builder.build())
            .await
            .map_err(|e| format!("Screenshot: {e}"))
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
        self.0
            .click(chromiumoxide::layout::Point { x, y })
            .await
            .map_err(|e| format!("{e}"))
            .map(|_| ())
    }

    pub async fn click_and_drag(&self, from: (f64, f64), to: (f64, f64)) -> Result<(), String> {
        self.0
            .click_and_drag(
                chromiumoxide::layout::Point {
                    x: from.0,
                    y: from.1,
                },
                chromiumoxide::layout::Point { x: to.0, y: to.1 },
            )
            .await
            .map_err(|e| format!("{e}"))
            .map(|_| ())
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
        // Step 1: Scroll into view (ignore error)
        let _ = self.element.scroll_into_view().await;

        // Step 2: Try standard chromiumoxide click
        if self.element.click().await.is_ok() {
            return Ok(());
        }

        // Step 3: Get center via JS getBoundingClientRect, then coordinate click
        // Tag the element with a unique attribute so we can query it from page context
        let tag = format!("__fd_click_{}", std::process::id());
        let _ = self.element.call_js_fn(
            &format!("function() {{ this.setAttribute('data-fd-click', '{tag}'); }}"),
            false,
        ).await;
        let js = format!(
            "(function(){{ var e=document.querySelector('[data-fd-click=\"{tag}\"]'); if(!e) return null; var r=e.getBoundingClientRect(); e.removeAttribute('data-fd-click'); return JSON.stringify({{x:r.x,y:r.y,w:r.width,h:r.height}}); }})()"
        );
        if let Ok(val) = self.page.evaluate(js).await {
            if let Some(s) = val.value().and_then(|v| v.as_str()) {
                if let Ok(rect) = serde_json::from_str::<serde_json::Value>(s) {
                    let x = rect["x"].as_f64().unwrap_or(0.0) + rect["w"].as_f64().unwrap_or(0.0) / 2.0;
                    let y = rect["y"].as_f64().unwrap_or(0.0) + rect["h"].as_f64().unwrap_or(0.0) / 2.0;
                    if self.page.click(chromiumoxide::layout::Point { x, y }).await.is_ok() {
                        return Ok(());
                    }
                }
            }
        }

        // Step 4: Final fallback - JS click
        let _ = self.element.call_js_fn("function() { this.click(); }", false).await;
        Ok(())
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
