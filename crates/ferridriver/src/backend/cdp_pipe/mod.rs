//! CDP Pipe backend -- Chrome DevTools Protocol over pipes (fd 3/4).
//!
//! Uses `--remote-debugging-pipe` flag to communicate with Chrome via
//! file descriptors instead of WebSocket. No port discovery, no handshake,
//! no framing overhead -- just NUL-delimited JSON over Unix pipes.
//!
//! Navigation follows Bun's ChromeBackend.cpp architecture: register a oneshot
//! waiter before sending Page.navigate, then await the waiter which resolves
//! when the reader task sees Page.loadEventFired for that session.

mod transport;
mod json_scan;

use super::*;
use rustc_hash::FxHashMap;
use std::time::Duration;
use transport::PipeTransport;

// ---- CdpPipeBrowser --------------------------------------------------------

pub struct CdpPipeBrowser {
    transport: Arc<PipeTransport>,
    child: tokio::process::Child,
    session_id: Option<String>,
    /// Track targetId -> sessionId for already-attached targets.
    attached_targets: std::sync::Mutex<FxHashMap<String, Option<String>>>,
}

impl CdpPipeBrowser {
    /// Enable required CDP domains on a session so events and queries work.
    async fn enable_domains(
        transport: &PipeTransport,
        session_id: Option<&str>,
    ) -> Result<(), String> {
        transport
            .send_command(session_id, "Page.enable", super::empty_params())
            .await?;
        transport
            .send_command(session_id, "Runtime.enable", super::empty_params())
            .await?;
        transport
            .send_command(session_id, "DOM.enable", super::empty_params())
            .await?;
        transport
            .send_command(session_id, "Network.enable", super::empty_params())
            .await?;
        transport
            .send_command(session_id, "Accessibility.enable", super::empty_params())
            .await?;

        // Inject selector engine on every new document so it's always available
        // without a separate evaluate call. Chrome runs this before any page JS.
        let engine_js = crate::selectors::build_inject_js();
        transport
            .send_command(
                session_id,
                "Page.addScriptToEvaluateOnNewDocument",
                serde_json::json!({"source": engine_js}),
            )
            .await?;

        Ok(())
    }

    /// Launch Chrome with `--remote-debugging-pipe` and communicate over fd 3/4.
    pub async fn launch(chromium_path: &str) -> Result<Self, String> {
        Self::launch_with_flags(chromium_path, &crate::state::chrome_flags(true, &[])).await
    }

    pub async fn launch_with_flags(chromium_path: &str, flags: &[String]) -> Result<Self, String> {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let user_data_dir =
            std::env::temp_dir().join(format!("ferridriver-pipe-{}-{id}", std::process::id()));

        let (transport, child) =
            PipeTransport::spawn(chromium_path, &user_data_dir, flags).await?;
        let transport = Arc::new(transport);

        // Enable target discovery so we get notified about new targets
        transport
            .send_command(
                None,
                "Target.setDiscoverTargets",
                serde_json::json!({"discover": true}),
            )
            .await?;

        // With --no-startup-window, Chrome won't create a default page target.
        // Create our own initial page target.
        let create_result = transport
            .send_command(
                None,
                "Target.createTarget",
                serde_json::json!({"url": "about:blank"}),
            )
            .await?;

        let target_id = create_result
            .get("targetId")
            .and_then(|v| v.as_str())
            .ok_or("No targetId from Target.createTarget")?
            .to_string();

        // Attach to the target to get a session ID
        let attach_result = transport
            .send_command(
                None,
                "Target.attachToTarget",
                serde_json::json!({"targetId": target_id, "flatten": true}),
            )
            .await?;

        let session_id = attach_result
            .get("sessionId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Enable required domains on the new session
        Self::enable_domains(&transport, session_id.as_deref()).await?;

        let mut attached = FxHashMap::default();
        attached.insert(target_id, session_id.clone());

        Ok(Self {
            transport,
            child,
            session_id,
            attached_targets: std::sync::Mutex::new(attached),
        })
    }

    pub async fn pages(&self) -> Result<Vec<AnyPage>, String> {
        let result = self
            .transport
            .send_command(None, "Target.getTargets", super::empty_params())
            .await?;

        let targets = result
            .get("targetInfos")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();

        let mut pages = Vec::new();
        for target in targets {
            if target.get("type").and_then(|v| v.as_str()) != Some("page") {
                continue;
            }
            let target_id = target
                .get("targetId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            // Check if we already have a session for this target
            let existing_sid = {
                self.attached_targets
                    .lock()
                    .unwrap()
                    .get(&target_id)
                    .cloned()
            };

            let sid = if let Some(sid) = existing_sid {
                // Already attached, reuse the session
                sid
            } else {
                let attach = self
                    .transport
                    .send_command(
                        None,
                        "Target.attachToTarget",
                        serde_json::json!({"targetId": target_id, "flatten": true}),
                    )
                    .await?;

                let sid = attach
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                // Track it as attached with its session
                self.attached_targets
                    .lock()
                    .unwrap()
                    .insert(target_id.clone(), sid.clone());

                // Enable domains on the new session
                Self::enable_domains(&self.transport, sid.as_deref()).await?;

                sid
            };

            pages.push(AnyPage::CdpPipe(CdpPipePage {
                transport: self.transport.clone(),
                session_id: sid,
                target_id,
            }));
        }
        Ok(pages)
    }

    pub async fn new_page(&self, url: &str) -> Result<AnyPage, String> {
        // Create target with about:blank initially so we can set up domains before navigation
        let result = self
            .transport
            .send_command(
                None,
                "Target.createTarget",
                serde_json::json!({"url": "about:blank"}),
            )
            .await?;

        let target_id = result
            .get("targetId")
            .and_then(|v| v.as_str())
            .ok_or("No targetId")?
            .to_string();

        let attach = self
            .transport
            .send_command(
                None,
                "Target.attachToTarget",
                serde_json::json!({"targetId": target_id, "flatten": true}),
            )
            .await?;

        let sid = attach
            .get("sessionId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Track as attached
        self.attached_targets
            .lock()
            .unwrap()
            .insert(target_id.clone(), sid.clone());

        // Enable domains BEFORE any navigation
        Self::enable_domains(&self.transport, sid.as_deref()).await?;

        let page = CdpPipePage {
            transport: self.transport.clone(),
            session_id: sid,
            target_id,
        };

        // Navigate if a real URL was requested (not about:blank)
        if url != "about:blank" && !url.is_empty() {
            page.goto(url).await?;
        }

        Ok(AnyPage::CdpPipe(page))
    }

    pub async fn new_page_isolated(&self, url: &str) -> Result<AnyPage, String> {
        // Create isolated browser context
        let ctx = self
            .transport
            .send_command(
                None,
                "Target.createBrowserContext",
                super::empty_params(),
            )
            .await?;

        let ctx_id = ctx
            .get("browserContextId")
            .and_then(|v| v.as_str())
            .ok_or("No browserContextId")?
            .to_string();

        // Create target in the isolated context, starting with about:blank
        let result = self
            .transport
            .send_command(
                None,
                "Target.createTarget",
                serde_json::json!({"url": "about:blank", "browserContextId": ctx_id}),
            )
            .await?;

        let target_id = result
            .get("targetId")
            .and_then(|v| v.as_str())
            .ok_or("No targetId")?
            .to_string();

        let attach = self
            .transport
            .send_command(
                None,
                "Target.attachToTarget",
                serde_json::json!({"targetId": target_id, "flatten": true}),
            )
            .await?;

        let sid = attach
            .get("sessionId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Track as attached
        self.attached_targets
            .lock()
            .unwrap()
            .insert(target_id.clone(), sid.clone());

        // Enable domains BEFORE any navigation
        Self::enable_domains(&self.transport, sid.as_deref()).await?;

        let page = CdpPipePage {
            transport: self.transport.clone(),
            session_id: sid,
            target_id,
        };

        // Navigate if a real URL was requested
        if url != "about:blank" && !url.is_empty() {
            page.goto(url).await?;
        }

        Ok(AnyPage::CdpPipe(page))
    }

    pub async fn close(&mut self) -> Result<(), String> {
        let _ = self
            .transport
            .send_command(None, "Browser.close", super::empty_params())
            .await;
        let _ = self.child.kill().await;
        Ok(())
    }
}

// ---- CdpPipePage ------------------------------------------------------------

#[derive(Clone)]
pub struct CdpPipePage {
    transport: Arc<PipeTransport>,
    session_id: Option<String>,
    target_id: String,
}

impl CdpPipePage {
    /// Send a CDP command to this page's session.
    async fn cmd(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.transport
            .send_command(self.session_id.as_deref(), method, params)
            .await
    }

    // ---- Navigation ----

    pub async fn goto(&self, url: &str) -> Result<(), String> {
        // Playwright approach: register lifecycle waiter, send Page.navigate,
        // wait for the matching lifecycle event (load by default).
        let rx = self
            .transport
            .register_nav_waiter(self.session_id.as_deref().unwrap_or(""))
            .await;

        let nav_result = self
            .cmd("Page.navigate", serde_json::json!({"url": url}))
            .await?;

        if let Some(error_text) = nav_result.get("errorText").and_then(|v| v.as_str()) {
            if !error_text.is_empty() {
                return Err(format!("Navigation failed: {error_text}"));
            }
        }

        // Wait for lifecycle event. Chrome fires domContentEventFired then
        // loadEventFired. We resolve on whichever comes first (already configured
        // in the transport reader). 30s timeout like Playwright.
        match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Ok(()),
            Err(_) => Ok(()),
        }
    }

    pub async fn wait_for_navigation(&self) -> Result<(), String> {
        // Register nav waiter and await Page.loadEventFired (Bun's pattern)
        let rx = self
            .transport
            .register_nav_waiter(self.session_id.as_deref().unwrap_or(""))
            .await;

        match tokio::time::timeout(Duration::from_secs(30), rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("Navigation waiter dropped".into()),
            Err(_) => Ok(()), // Timeout, proceed anyway
        }
    }

    pub async fn reload(&self) -> Result<(), String> {
        self.cmd("Page.reload", super::empty_params()).await?;
        Ok(())
    }

    pub async fn go_back(&self) -> Result<(), String> {
        self.history_go(-1).await
    }

    pub async fn go_forward(&self) -> Result<(), String> {
        self.history_go(1).await
    }

    /// Navigate history by delta. Same as Bun's historyGo:
    /// Page.getNavigationHistory -> pick entries[currentIndex + delta].id
    /// -> Page.navigateToHistoryEntry -> Page.loadEventFired settles.
    async fn history_go(&self, delta: i32) -> Result<(), String> {
        let hist = self.cmd("Page.getNavigationHistory", super::empty_params()).await?;
        let current = hist.get("currentIndex").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        let target = current + delta;
        let entries = hist.get("entries").and_then(|v| v.as_array());
        let Some(entries) = entries else { return Ok(()); };
        // At history boundary — nothing to do (same as Bun's canGoBack check)
        if target < 0 || target as usize >= entries.len() { return Ok(()); }
        let entry_id = entries[target as usize].get("id").and_then(|v| v.as_i64()).unwrap_or(0);

        // Register nav waiter before navigating
        let rx = self.transport
            .register_nav_waiter(self.session_id.as_deref().unwrap_or(""))
            .await;
        self.cmd("Page.navigateToHistoryEntry", serde_json::json!({"entryId": entry_id})).await?;
        // Wait for Page.loadEventFired — the navigation IS happening so this will fire
        match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
            Ok(Ok(r)) => r,
            _ => Ok(()),
        }
    }

    pub async fn url(&self) -> Result<Option<String>, String> {
        let result = self
            .cmd(
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": "location.href",
                    "returnByValue": true,
                }),
            )
            .await?;
        Ok(result
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()))
    }

    pub async fn title(&self) -> Result<Option<String>, String> {
        let result = self
            .cmd(
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": "document.title",
                    "returnByValue": true,
                }),
            )
            .await?;
        Ok(result
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()))
    }

    // ---- JavaScript ----

    pub async fn evaluate(&self, expression: &str) -> Result<Option<serde_json::Value>, String> {
        let result = self
            .cmd(
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
            )
            .await?;

        if let Some(exception) = result.get("exceptionDetails") {
            let text = exception
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("Evaluation error");
            return Err(text.to_string());
        }

        Ok(result
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned())
    }

    // ---- Elements ----

    pub async fn find_element(&self, selector: &str) -> Result<AnyElement, String> {
        // Get a fresh document root each time, since nodeIds get invalidated
        // after navigation or DOM changes.
        let doc = self
            .cmd("DOM.getDocument", serde_json::json!({"depth": 0}))
            .await?;
        let root_id = doc
            .get("root")
            .and_then(|r| r.get("nodeId"))
            .and_then(|v| v.as_i64())
            .ok_or_else(|| "No document root".to_string())?;

        // Query selector
        let result = self
            .cmd(
                "DOM.querySelector",
                serde_json::json!({"nodeId": root_id, "selector": selector}),
            )
            .await?;

        let node_id = result
            .get("nodeId")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| format!("'{selector}' not found"))?;

        if node_id == 0 {
            return Err(format!("'{selector}' not found"));
        }

        Ok(AnyElement::CdpPipe(CdpPipeElement {
            transport: self.transport.clone(),
            session_id: self.session_id.clone(),
            node_id,
        }))
    }

    /// Evaluate JS that returns a DOM element. Uses Runtime.evaluate without
    /// returnByValue to get an objectId, then DOM.requestNode for the nodeId.
    /// Single evaluate + one DOM call = 2 round-trips (vs 5 for tag-and-query).
    pub async fn evaluate_to_element(&self, js: &str) -> Result<AnyElement, String> {
        // Ensure DOM agent has the document tree (required for DOM.requestNode)
        let _ = self.cmd("DOM.getDocument", serde_json::json!({"depth": 0})).await;

        let result = self
            .cmd(
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": js,
                    "returnByValue": false,
                }),
            )
            .await?;

        let object_id = result
            .get("result")
            .and_then(|r| r.get("objectId"))
            .and_then(|v| v.as_str())
            .ok_or("JS did not return a DOM element")?;

        let node_result = self
            .cmd(
                "DOM.requestNode",
                serde_json::json!({"objectId": object_id}),
            )
            .await?;

        let node_id = node_result
            .get("nodeId")
            .and_then(|v| v.as_i64())
            .ok_or("Could not resolve element nodeId")?;

        if node_id == 0 {
            return Err("Element not found".into());
        }

        Ok(AnyElement::CdpPipe(CdpPipeElement {
            transport: self.transport.clone(),
            session_id: self.session_id.clone(),
            node_id,
        }))
    }

    // ---- Content ----

    pub async fn content(&self) -> Result<String, String> {
        let result = self
            .cmd(
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": "document.documentElement.outerHTML",
                    "returnByValue": true,
                }),
            )
            .await?;
        Ok(result
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub async fn set_content(&self, html: &str) -> Result<(), String> {
        // Get the frame tree to find the main frame ID
        let tree = self
            .cmd("Page.getFrameTree", super::empty_params())
            .await?;
        let frame_id = tree
            .get("frameTree")
            .and_then(|f| f.get("frame"))
            .and_then(|f| f.get("id"))
            .and_then(|v| v.as_str())
            .ok_or("No main frame")?;

        // Embed the selector engine directly in the HTML as a <script> tag.
        // This avoids a separate evaluate round-trip after setDocumentContent.
        let engine_js = crate::selectors::build_inject_js();
        let augmented = format!("<script>{engine_js}</script>{html}");
        self.cmd(
            "Page.setDocumentContent",
            serde_json::json!({"frameId": frame_id, "html": augmented}),
        )
        .await?;
        Ok(())
    }

    // ---- Screenshots ----

    pub async fn screenshot(&self, opts: ScreenshotOpts) -> Result<Vec<u8>, String> {
        let format_str = match opts.format {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpeg",
            ImageFormat::Webp => "webp",
        };
        let mut params = serde_json::json!({"format": format_str, "optimizeForSpeed": true});
        if let Some(q) = opts.quality {
            params["quality"] = serde_json::json!(q);
        }
        if opts.full_page {
            // Get full page dimensions
            let metrics = self
                .cmd(
                    "Page.getLayoutMetrics",
                    super::empty_params(),
                )
                .await?;
            if let Some(content_size) = metrics.get("contentSize") {
                let w = content_size.get("width").and_then(|v| v.as_f64()).unwrap_or(800.0);
                let h = content_size.get("height").and_then(|v| v.as_f64()).unwrap_or(600.0);
                params["clip"] = serde_json::json!({
                    "x": 0, "y": 0, "width": w, "height": h, "scale": 1
                });
            }
        }

        let result = self
            .cmd("Page.captureScreenshot", params)
            .await?;
        let data = result
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or("No screenshot data")?;
        base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            data,
        )
        .map_err(|e| format!("Decode screenshot: {e}"))
    }

    pub async fn screenshot_element(
        &self,
        selector: &str,
        format: ImageFormat,
    ) -> Result<Vec<u8>, String> {
        // Get element bounding box via JS
        let js = format!(
            r#"(function(){{
                const el = document.querySelector('{}');
                if (!el) return null;
                const r = el.getBoundingClientRect();
                return JSON.stringify({{x:r.x,y:r.y,width:r.width,height:r.height}});
            }})()"#,
            selector.replace('\'', "\\'")
        );
        let result = self.evaluate(&js).await?;
        let rect_str = result
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .ok_or_else(|| format!("'{selector}' not found"))?;
        let rect: serde_json::Value =
            serde_json::from_str(&rect_str).map_err(|e| format!("Parse rect: {e}"))?;

        let format_str = match format {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpeg",
            ImageFormat::Webp => "webp",
        };

        let result = self
            .cmd(
                "Page.captureScreenshot",
                serde_json::json!({
                    "format": format_str,
                    "clip": {
                        "x": rect["x"], "y": rect["y"],
                        "width": rect["width"], "height": rect["height"],
                        "scale": 1
                    }
                }),
            )
            .await?;
        let data = result
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or("No screenshot data")?;
        base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            data,
        )
        .map_err(|e| format!("Decode: {e}"))
    }

    // ---- PDF ----

    pub async fn pdf(&self, landscape: bool, print_background: bool) -> Result<Vec<u8>, String> {
        let result = self
            .cmd(
                "Page.printToPDF",
                serde_json::json!({
                    "landscape": landscape,
                    "printBackground": print_background,
                    "preferCSSPageSize": true,
                }),
            )
            .await?;
        let data = result
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or("No PDF data")?;
        base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            data,
        )
        .map_err(|e| format!("Decode PDF: {e}"))
    }

    // ---- File upload ----

    pub async fn set_file_input(&self, selector: &str, paths: &[String]) -> Result<(), String> {
        // Get document root
        let doc = self.cmd("DOM.getDocument", super::empty_params()).await?;
        let root_id = doc.get("root").and_then(|r| r.get("nodeId")).and_then(|v| v.as_i64())
            .ok_or("No document root")?;

        // Query for element
        let query = self.cmd("DOM.querySelector", serde_json::json!({
            "nodeId": root_id,
            "selector": selector
        })).await?;
        let node_id = query.get("nodeId").and_then(|v| v.as_i64()).ok_or("Element not found")?;

        // Get backendNodeId
        let desc = self.cmd("DOM.describeNode", serde_json::json!({"nodeId": node_id})).await?;
        let backend_node_id = desc.get("node").and_then(|n| n.get("backendNodeId")).and_then(|v| v.as_i64())
            .ok_or("No backendNodeId")?;

        // Set files
        self.cmd("DOM.setFileInputFiles", serde_json::json!({
            "files": paths,
            "backendNodeId": backend_node_id
        })).await?;
        Ok(())
    }

    // ---- Accessibility ----

    pub async fn accessibility_tree(&self) -> Result<Vec<AxNodeData>, String> {
        let result = self
            .cmd(
                "Accessibility.getFullAXTree",
                serde_json::json!({"depth": -1}),
            )
            .await?;

        let nodes = result
            .get("nodes")
            .and_then(|n| n.as_array())
            .ok_or("No a11y nodes")?;

        Ok(nodes
            .iter()
            .map(|node| {
                let get_ax_value =
                    |field: &str| -> Option<String> {
                        node.get(field)
                            .and_then(|v| v.get("value"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    };

                let properties = node
                    .get("properties")
                    .and_then(|p| p.as_array())
                    .map(|props| {
                        props
                            .iter()
                            .map(|p| AxProperty {
                                name: p
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_lowercase(),
                                value: p.get("value").and_then(|v| v.get("value")).cloned(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                AxNodeData {
                    node_id: node
                        .get("nodeId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    parent_id: node
                        .get("parentId")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    backend_dom_node_id: node
                        .get("backendDOMNodeId")
                        .and_then(|v| v.as_i64()),
                    ignored: node
                        .get("ignored")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    role: get_ax_value("role"),
                    name: get_ax_value("name"),
                    description: get_ax_value("description"),
                    properties,
                }
            })
            .collect())
    }

    // ---- Input ----

    pub async fn click_at(&self, x: f64, y: f64) -> Result<(), String> {
        self.click_at_opts(x, y, "left", 1).await
    }

    pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<(), String> {
        self.cmd(
            "Input.dispatchMouseEvent",
            serde_json::json!({"type": "mousePressed", "x": x, "y": y, "button": button, "clickCount": click_count}),
        )
        .await?;
        self.cmd(
            "Input.dispatchMouseEvent",
            serde_json::json!({"type": "mouseReleased", "x": x, "y": y, "button": button, "clickCount": click_count}),
        )
        .await?;
        Ok(())
    }

    pub async fn move_mouse(&self, x: f64, y: f64) -> Result<(), String> {
        self.cmd(
            "Input.dispatchMouseEvent",
            serde_json::json!({"type": "mouseMoved", "x": x, "y": y}),
        )
        .await?;
        Ok(())
    }

    pub async fn move_mouse_smooth(&self, from_x: f64, from_y: f64, to_x: f64, to_y: f64, steps: u32) -> Result<(), String> {
        let steps = steps.max(1);
        for i in 0..=steps {
            let t = i as f64 / steps as f64;
            let ease = t * t * (3.0 - 2.0 * t);
            let x = from_x + (to_x - from_x) * ease;
            let y = from_y + (to_y - from_y) * ease;
            self.cmd(
                "Input.dispatchMouseEvent",
                serde_json::json!({"type": "mouseMoved", "x": x, "y": y}),
            )
            .await?;
        }
        Ok(())
    }

    pub async fn click_and_drag(
        &self,
        from: (f64, f64),
        to: (f64, f64),
    ) -> Result<(), String> {
        self.cmd(
            "Input.dispatchMouseEvent",
            serde_json::json!({"type": "mousePressed", "x": from.0, "y": from.1, "button": "left", "clickCount": 1}),
        )
        .await?;
        let steps = 10u32;
        for i in 1..=steps {
            let t = i as f64 / steps as f64;
            let ease = t * t * (3.0 - 2.0 * t);
            let x = from.0 + (to.0 - from.0) * ease;
            let y = from.1 + (to.1 - from.1) * ease;
            self.cmd(
                "Input.dispatchMouseEvent",
                serde_json::json!({"type": "mouseMoved", "x": x, "y": y, "button": "left"}),
            )
            .await?;
        }
        self.cmd(
            "Input.dispatchMouseEvent",
            serde_json::json!({"type": "mouseReleased", "x": to.0, "y": to.1, "button": "left", "clickCount": 1}),
        )
        .await?;
        Ok(())
    }

    pub async fn mouse_wheel(&self, delta_x: f64, delta_y: f64) -> Result<(), String> {
        self.cmd("Input.dispatchMouseEvent",
            serde_json::json!({"type": "mouseWheel", "x": 0, "y": 0, "deltaX": delta_x, "deltaY": delta_y}),
        ).await?;
        Ok(())
    }

    pub async fn mouse_down(&self, x: f64, y: f64, button: &str) -> Result<(), String> {
        self.cmd("Input.dispatchMouseEvent",
            serde_json::json!({"type": "mousePressed", "x": x, "y": y, "button": button, "clickCount": 1}),
        ).await?;
        Ok(())
    }

    pub async fn mouse_up(&self, x: f64, y: f64, button: &str) -> Result<(), String> {
        self.cmd("Input.dispatchMouseEvent",
            serde_json::json!({"type": "mouseReleased", "x": x, "y": y, "button": button, "clickCount": 1}),
        ).await?;
        Ok(())
    }

    pub async fn type_str(&self, text: &str) -> Result<(), String> {
        for ch in text.chars() {
            self.cmd(
                "Input.dispatchKeyEvent",
                serde_json::json!({"type": "char", "text": ch.to_string()}),
            )
            .await?;
        }
        Ok(())
    }

    pub async fn press_key(&self, key: &str) -> Result<(), String> {
        // Port of Bun's cdpKeyInfo table: map key names to DOM key string,
        // Windows VK code, and text character. Text-producing keys use "keyDown",
        // control keys use "rawKeyDown".
        let (dom_key, vk, text) = match key {
            "Enter"      => ("Enter", 13, Some("\r")),
            "Tab"        => ("Tab", 9, Some("\t")),
            "Space" | " "=> (" ", 32, Some(" ")),
            "Backspace"  => ("Backspace", 8, None),
            "Delete"     => ("Delete", 46, None),
            "Escape"     => ("Escape", 27, None),
            "ArrowLeft"  => ("ArrowLeft", 37, None),
            "ArrowRight" => ("ArrowRight", 39, None),
            "ArrowUp"    => ("ArrowUp", 38, None),
            "ArrowDown"  => ("ArrowDown", 40, None),
            "Home"       => ("Home", 36, None),
            "End"        => ("End", 35, None),
            "PageUp"     => ("PageUp", 33, None),
            "PageDown"   => ("PageDown", 34, None),
            ch           => (ch, 0, if ch.len() == 1 { Some(ch) } else { None }),
        };

        let down_type = if text.is_some() { "keyDown" } else { "rawKeyDown" };
        let mut down_params = serde_json::json!({
            "type": down_type, "key": dom_key,
            "windowsVirtualKeyCode": vk,
        });
        if let Some(t) = text {
            down_params["text"] = serde_json::json!(t);
        }

        self.cmd("Input.dispatchKeyEvent", down_params).await?;
        self.cmd("Input.dispatchKeyEvent", serde_json::json!({
            "type": "keyUp", "key": dom_key,
            "windowsVirtualKeyCode": vk,
        })).await?;
        Ok(())
    }

    // ---- Cookies ----

    pub async fn get_cookies(&self) -> Result<Vec<CookieData>, String> {
        let result = self
            .cmd("Network.getCookies", super::empty_params())
            .await?;
        let cookies = result
            .get("cookies")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(cookies
            .iter()
            .map(|c| CookieData {
                name: c.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                value: c.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                domain: c.get("domain").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                path: c.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                secure: c.get("secure").and_then(|v| v.as_bool()).unwrap_or(false),
                http_only: c.get("httpOnly").and_then(|v| v.as_bool()).unwrap_or(false),
                expires: c.get("expires").and_then(|v| v.as_f64()),
            })
            .collect())
    }

    pub async fn set_cookie(&self, cookie: CookieData) -> Result<(), String> {
        let mut params = serde_json::json!({
            "name": cookie.name,
            "value": cookie.value,
        });
        if !cookie.domain.is_empty() {
            params["domain"] = serde_json::json!(cookie.domain);
        }
        if !cookie.path.is_empty() {
            params["path"] = serde_json::json!(cookie.path);
        }
        params["secure"] = serde_json::json!(cookie.secure);
        params["httpOnly"] = serde_json::json!(cookie.http_only);
        if let Some(e) = cookie.expires {
            params["expires"] = serde_json::json!(e);
        }
        self.cmd("Network.setCookie", params).await?;
        Ok(())
    }

    pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<(), String> {
        let mut params = serde_json::json!({"name": name});
        if let Some(d) = domain {
            params["domain"] = serde_json::json!(d);
        } else {
            // Chrome requires at least url or domain for Network.deleteCookies
            if let Ok(Some(url)) = self.url().await {
                params["url"] = serde_json::json!(url);
            }
        }
        self.cmd("Network.deleteCookies", params).await?;
        Ok(())
    }

    pub async fn clear_cookies(&self) -> Result<(), String> {
        self.cmd("Storage.clearCookies", super::empty_params()).await?;
        Ok(())
    }

    // ---- Emulation ----

    pub async fn emulate_viewport(&self, config: &crate::options::ViewportConfig) -> Result<(), String> {
        let _ = self.cmd("Emulation.clearDeviceMetricsOverride", super::empty_params()).await;
        let is_landscape = config.is_landscape || config.width > config.height;
        let orientation = if config.is_mobile {
            if is_landscape {
                serde_json::json!({"angle": 90, "type": "landscapePrimary"})
            } else {
                serde_json::json!({"angle": 0, "type": "portraitPrimary"})
            }
        } else {
            serde_json::json!({"angle": 0, "type": "landscapePrimary"})
        };
        self.cmd(
            "Emulation.setDeviceMetricsOverride",
            serde_json::json!({
                "width": config.width,
                "height": config.height,
                "deviceScaleFactor": config.device_scale_factor,
                "mobile": config.is_mobile,
                "screenWidth": config.width,
                "screenHeight": config.height,
                "screenOrientation": orientation,
            }),
        )
        .await?;
        if config.has_touch {
            let _ = self.cmd(
                "Emulation.setTouchEmulationEnabled",
                serde_json::json!({"enabled": true, "maxTouchPoints": 5}),
            ).await;
        }
        Ok(())
    }

    pub async fn set_user_agent(&self, ua: &str) -> Result<(), String> {
        self.cmd(
            "Network.setUserAgentOverride",
            serde_json::json!({"userAgent": ua}),
        )
        .await?;
        Ok(())
    }

    pub async fn set_geolocation(
        &self,
        lat: f64,
        lng: f64,
        accuracy: f64,
    ) -> Result<(), String> {
        self.cmd(
            "Emulation.setGeolocationOverride",
            serde_json::json!({
                "latitude": lat, "longitude": lng, "accuracy": accuracy,
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn set_locale(&self, locale: &str) -> Result<(), String> {
        // Playwright approach: use Emulation.setLocaleOverride for Intl APIs,
        // AND Network.setUserAgentOverride with acceptLanguage for navigator.language.
        let _ = self.cmd("Emulation.setLocaleOverride", serde_json::json!({"locale": locale})).await;
        self.cmd("Network.setUserAgentOverride", serde_json::json!({
            "userAgent": "",
            "acceptLanguage": locale,
        })).await?;
        Ok(())
    }

    pub async fn set_timezone(&self, timezone_id: &str) -> Result<(), String> {
        self.cmd("Emulation.setTimezoneOverride", serde_json::json!({"timezoneId": timezone_id})).await?;
        Ok(())
    }

    pub async fn emulate_media(&self, opts: &crate::options::EmulateMediaOptions) -> Result<(), String> {
        let mut features = Vec::new();
        if let Some(cs) = &opts.color_scheme {
            features.push(serde_json::json!({"name": "prefers-color-scheme", "value": cs}));
        }
        if let Some(rm) = &opts.reduced_motion {
            features.push(serde_json::json!({"name": "prefers-reduced-motion", "value": rm}));
        }
        if let Some(fc) = &opts.forced_colors {
            features.push(serde_json::json!({"name": "forced-colors", "value": fc}));
        }
        if let Some(c) = &opts.contrast {
            features.push(serde_json::json!({"name": "prefers-contrast", "value": c}));
        }
        let mut params = serde_json::json!({"features": features});
        if let Some(media) = &opts.media {
            params["media"] = serde_json::json!(media);
        }
        self.cmd("Emulation.setEmulatedMedia", params).await?;
        Ok(())
    }

    pub async fn set_javascript_enabled(&self, enabled: bool) -> Result<(), String> {
        self.cmd("Emulation.setScriptExecutionDisabled", serde_json::json!({"value": !enabled})).await?;
        Ok(())
    }

    pub async fn set_extra_http_headers(&self, headers: &rustc_hash::FxHashMap<String, String>) -> Result<(), String> {
        let h: serde_json::Map<String, serde_json::Value> = headers.iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        self.cmd("Network.setExtraHTTPHeaders", serde_json::json!({"headers": h})).await?;
        Ok(())
    }

    pub async fn grant_permissions(&self, permissions: &[String], origin: Option<&str>) -> Result<(), String> {
        let mut params = serde_json::json!({"permissions": permissions});
        if let Some(o) = origin {
            params["origin"] = serde_json::json!(o);
        }
        self.cmd("Browser.grantPermissions", params).await?;
        Ok(())
    }

    pub async fn reset_permissions(&self) -> Result<(), String> {
        self.cmd("Browser.resetPermissions", super::empty_params()).await?;
        Ok(())
    }

    pub async fn set_focus_emulation_enabled(&self, enabled: bool) -> Result<(), String> {
        self.cmd("Emulation.setFocusEmulationEnabled", serde_json::json!({"enabled": enabled})).await?;
        Ok(())
    }

    // ---- Network ----

    pub async fn set_network_state(
        &self,
        offline: bool,
        latency: f64,
        download: f64,
        upload: f64,
    ) -> Result<(), String> {
        self.cmd(
            "Network.emulateNetworkConditions",
            serde_json::json!({
                "offline": offline,
                "latency": latency,
                "downloadThroughput": download,
                "uploadThroughput": upload,
            }),
        )
        .await?;
        Ok(())
    }

    // ---- Tracing ----

    pub async fn start_tracing(&self) -> Result<(), String> {
        self.cmd("Tracing.start", super::empty_params()).await?;
        Ok(())
    }

    pub async fn stop_tracing(&self) -> Result<(), String> {
        self.cmd("Tracing.end", super::empty_params()).await?;
        Ok(())
    }

    pub async fn metrics(&self) -> Result<Vec<MetricData>, String> {
        let result = self
            .cmd("Performance.getMetrics", super::empty_params())
            .await?;
        let metrics = result
            .get("metrics")
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(metrics
            .iter()
            .map(|m| MetricData {
                name: m.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                value: m.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0),
            })
            .collect())
    }

    // ---- Ref resolution ----

    pub async fn resolve_backend_node(
        &self,
        backend_node_id: i64,
        ref_id: &str,
    ) -> Result<AnyElement, String> {
        let resolve_result = self
            .cmd(
                "DOM.resolveNode",
                serde_json::json!({"backendNodeId": backend_node_id}),
            )
            .await?;

        let object_id = resolve_result
            .get("object")
            .and_then(|o| o.get("objectId"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("Ref '{ref_id}' no longer valid."))?;

        // Tag element with data-cref attribute
        self.cmd(
            "Runtime.callFunctionOn",
            serde_json::json!({
                "objectId": object_id,
                "functionDeclaration": format!("function() {{ this.setAttribute('data-cref', '{ref_id}'); }}")
            }),
        )
        .await?;

        // Find by the tag
        self.find_element(&format!("[data-cref='{ref_id}']")).await
    }

    // ---- Event listeners ----

    pub fn attach_listeners(
        &self,
        console_log: Arc<RwLock<Vec<ConsoleMsg>>>,
        network_log: Arc<RwLock<Vec<NetRequest>>>,
        dialog_log: Arc<RwLock<Vec<crate::state::DialogEvent>>>,
    ) {
        // Domains are already enabled via enable_domains() at session creation time.
        // No need to re-enable here.

        let transport = self.transport.clone();
        let session_id = self.session_id.clone();

        // Console listener
        let cl = console_log;
        let t = transport.clone();
        let sid = session_id.clone();
        tokio::spawn(async move {
            let mut rx = t.subscribe_events();
            while let Ok(event) = rx.recv().await {
                // Filter by session
                if let Some(ref expected_sid) = sid {
                    let event_sid = event.get("sessionId").and_then(|v| v.as_str());
                    if event_sid != Some(expected_sid.as_str()) {
                        continue;
                    }
                }

                if event.get("method").and_then(|m| m.as_str())
                    == Some("Runtime.consoleAPICalled")
                {
                    if let Some(params) = event.get("params") {
                        let level = params
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("log")
                            .to_string();
                        let text = params
                            .get("args")
                            .and_then(|a| a.as_array())
                            .map(|args| {
                                args.iter()
                                    .filter_map(|a| {
                                        a.get("value")
                                            .map(|v| v.to_string().trim_matches('"').to_string())
                                    })
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            })
                            .unwrap_or_default();
                        cl.write().await.push(ConsoleMsg { level, text });
                    }
                }
            }
        });

        // Network listener
        let nl = network_log;
        let t = transport;
        let sid = session_id;
        tokio::spawn(async move {
            let mut rx = t.subscribe_events();
            while let Ok(event) = rx.recv().await {
                // Filter by session
                if let Some(ref expected_sid) = sid {
                    let event_sid = event.get("sessionId").and_then(|v| v.as_str());
                    if event_sid != Some(expected_sid.as_str()) {
                        continue;
                    }
                }

                let method = event
                    .get("method")
                    .and_then(|m| m.as_str())
                    .unwrap_or("");
                match method {
                    "Network.requestWillBeSent" => {
                        if let Some(params) = event.get("params") {
                            let id = params
                                .get("requestId")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let req = params.get("request");
                            nl.write().await.push(NetRequest {
                                id,
                                method: req
                                    .and_then(|r| r.get("method"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                url: req
                                    .and_then(|r| r.get("url"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                resource_type: params
                                    .get("type")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                status: None,
                                mime_type: None,
                            });
                        }
                    }
                    "Network.responseReceived" => {
                        if let Some(params) = event.get("params") {
                            let rid = params
                                .get("requestId")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let resp = params.get("response");
                            let status = resp.and_then(|r| r.get("status")).and_then(|v| v.as_i64());
                            let mime = resp
                                .and_then(|r| r.get("mimeType"))
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            let mut reqs = nl.write().await;
                            if let Some(r) = reqs.iter_mut().rev().find(|r| r.id == rid) {
                                r.status = status;
                                r.mime_type = mime;
                            }
                        }
                    }
                    _ => {}
                }
            }
        });

        // Dialog auto-dismiss listener
        let dl = dialog_log;
        let t = self.transport.clone();
        let sid = self.session_id.clone();
        tokio::spawn(async move {
            let mut rx = t.subscribe_events();
            while let Ok(event) = rx.recv().await {
                if let Some(ref expected_sid) = sid {
                    let event_sid = event.get("sessionId").and_then(|v| v.as_str());
                    if event_sid != Some(expected_sid.as_str()) {
                        continue;
                    }
                }
                if event.get("method").and_then(|m| m.as_str()) == Some("Page.javascriptDialogOpening") {
                    if let Some(params) = event.get("params") {
                        let dialog_type = params.get("type").and_then(|v| v.as_str()).unwrap_or("alert");
                        let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let accept = dialog_type != "prompt";
                        // Dismiss the dialog
                        let _ = t.send_command(
                            sid.as_deref(),
                            "Page.handleJavaScriptDialog",
                            serde_json::json!({"accept": accept}),
                        ).await;
                        dl.write().await.push(crate::state::DialogEvent {
                            dialog_type: dialog_type.to_string(),
                            message,
                            action: if accept { "accepted".into() } else { "dismissed".into() },
                        });
                    }
                }
            }
        });
    }
}

// ---- CdpPipeElement ---------------------------------------------------------

#[derive(Clone)]
pub struct CdpPipeElement {
    transport: Arc<PipeTransport>,
    session_id: Option<String>,
    node_id: i64,
}

impl CdpPipeElement {
    async fn cmd(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.transport
            .send_command(self.session_id.as_deref(), method, params)
            .await
    }

    /// Get element center coordinates for clicking.
    async fn get_center(&self) -> Result<(f64, f64), String> {
        let result = self
            .cmd(
                "DOM.getBoxModel",
                serde_json::json!({"nodeId": self.node_id}),
            )
            .await?;
        let content = result
            .get("model")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
            .ok_or("No box model")?;

        // content quad: [x1,y1, x2,y2, x3,y3, x4,y4]
        if content.len() < 8 {
            return Err("Invalid box model".into());
        }
        let x1 = content[0].as_f64().unwrap_or(0.0);
        let y1 = content[1].as_f64().unwrap_or(0.0);
        let x3 = content[4].as_f64().unwrap_or(0.0);
        let y3 = content[5].as_f64().unwrap_or(0.0);

        Ok(((x1 + x3) / 2.0, (y1 + y3) / 2.0))
    }

    /// Resolve this element's nodeId to a Runtime objectId.
    async fn resolve_object_id(&self) -> Result<String, String> {
        let resolved = self
            .cmd("DOM.resolveNode", serde_json::json!({"nodeId": self.node_id}))
            .await?;
        resolved
            .get("object")
            .and_then(|o| o.get("objectId"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or("Cannot resolve element".into())
    }

    /// Call a JS function on this element and return the value.
    pub async fn call_js_fn_value(&self, function: &str) -> Result<Option<serde_json::Value>, String> {
        let object_id = self.resolve_object_id().await?;
        let result = self
            .cmd(
                "Runtime.callFunctionOn",
                serde_json::json!({
                    "objectId": object_id,
                    "functionDeclaration": function,
                    "returnByValue": true,
                }),
            )
            .await?;
        Ok(result
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned())
    }

    pub async fn click(&self) -> Result<(), String> {
        // Single JS call: scroll into view + get center coordinates
        let center = self.call_js_fn_value(
            "function() { this.scrollIntoViewIfNeeded(); var r = this.getBoundingClientRect(); return {x: r.x + r.width/2, y: r.y + r.height/2}; }"
        ).await?;

        if let Some(c) = center {
            let x = c.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = c.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            if x == 0.0 && y == 0.0 {
                // Element has no layout, use JS click
                return self.call_js_fn("function() { this.click(); }").await;
            }
            self.cmd(
                "Input.dispatchMouseEvent",
                serde_json::json!({"type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 1}),
            )
            .await?;
            self.cmd(
                "Input.dispatchMouseEvent",
                serde_json::json!({"type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 1}),
            )
            .await?;
            Ok(())
        } else {
            self.call_js_fn("function() { this.click(); }").await
        }
    }

    pub async fn hover(&self) -> Result<(), String> {
        self.scroll_into_view().await?;
        let (x, y) = self.get_center().await?;
        self.cmd(
            "Input.dispatchMouseEvent",
            serde_json::json!({"type": "mouseMoved", "x": x, "y": y}),
        )
        .await?;
        Ok(())
    }

    pub async fn type_str(&self, text: &str) -> Result<(), String> {
        self.click().await?;
        for ch in text.chars() {
            self.cmd(
                "Input.dispatchKeyEvent",
                serde_json::json!({"type": "char", "text": ch.to_string()}),
            )
            .await?;
        }
        Ok(())
    }

    pub async fn call_js_fn(&self, function: &str) -> Result<(), String> {
        let object_id = self.resolve_object_id().await?;
        self.cmd(
            "Runtime.callFunctionOn",
            serde_json::json!({
                "objectId": object_id,
                "functionDeclaration": function,
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn scroll_into_view(&self) -> Result<(), String> {
        self.cmd(
            "DOM.scrollIntoViewIfNeeded",
            serde_json::json!({"nodeId": self.node_id}),
        )
        .await?;
        Ok(())
    }

    pub async fn screenshot(&self, format: ImageFormat) -> Result<Vec<u8>, String> {
        // Get bounding box
        let result = self
            .cmd(
                "DOM.getBoxModel",
                serde_json::json!({"nodeId": self.node_id}),
            )
            .await?;
        let content = result
            .get("model")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
            .ok_or("No box model")?;

        if content.len() < 8 {
            return Err("Invalid box model".into());
        }

        let x = content[0].as_f64().unwrap_or(0.0);
        let y = content[1].as_f64().unwrap_or(0.0);
        let w = content[4].as_f64().unwrap_or(0.0) - x;
        let h = content[5].as_f64().unwrap_or(0.0) - y;

        let fmt = match format {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpeg",
            ImageFormat::Webp => "webp",
        };

        let result = self
            .cmd(
                "Page.captureScreenshot",
                serde_json::json!({
                    "format": fmt,
                    "clip": {"x": x, "y": y, "width": w, "height": h, "scale": 1}
                }),
            )
            .await?;
        let data = result
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or("No screenshot data")?;
        base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            data,
        )
        .map_err(|e| format!("Decode: {e}"))
    }
}
