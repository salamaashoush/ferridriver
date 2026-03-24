//! WebKit backend — native WKWebView on macOS.
//!
//! Architecture ported from Bun's webview implementation:
//! - Parent communicates over Unix socketpair with binary frames
//! - Child subprocess runs WKWebView on main thread (single-threaded, nonblocking)
//! - No JSON IPC. No tokio for spawning. No background threads in child.

pub mod ipc;

use super::*;
use ipc::{IpcClient, IpcResponse, Op};

// ─── WebKitBrowser ──────────────────────────────────────────────────────────

pub struct WebKitBrowser {
    client: Arc<IpcClient>,
    child: std::process::Child,
}

impl WebKitBrowser {
    pub async fn launch() -> Result<Self, String> {
        let (client, child) = IpcClient::spawn().await?;
        Ok(Self { client: Arc::new(client), child })
    }

    pub async fn pages(&self) -> Result<Vec<AnyPage>, String> {
        let r = self.client.send_empty(Op::ListViews).await?;
        match r {
            IpcResponse::ViewList(ids) => Ok(ids.into_iter().map(|id| {
                AnyPage::WebKit(WebKitPage { client: self.client.clone(), view_id: id })
            }).collect()),
            IpcResponse::Error(e) => Err(e),
            _ => Err("unexpected".into()),
        }
    }

    pub async fn new_page(&self, url: &str) -> Result<AnyPage, String> {
        let r = self.client.send_str(Op::CreateView, url).await?;
        match r {
            IpcResponse::ViewCreated(id) => {
                let page = WebKitPage { client: self.client.clone(), view_id: id };
                // Inject selector engine via WKUserScript (runs at document start
                // on every navigation, equivalent to addScriptToEvaluateOnNewDocument)
                let engine_js = crate::selectors::build_inject_js();
                let mut p = Vec::new();
                p.extend_from_slice(&page.vid().to_le_bytes());
                ipc::str_encode(&mut p, &engine_js);
                let _ = page.client.send(Op::AddInitScript, &p).await;
                Ok(AnyPage::WebKit(page))
            }
            IpcResponse::Error(e) => Err(e),
            _ => Err("unexpected".into()),
        }
    }

    pub async fn new_page_isolated(&self, url: &str) -> Result<AnyPage, String> {
        self.new_page(url).await
    }

    pub async fn close(&mut self) -> Result<(), String> {
        // OP_SHUTDOWN calls _exit(0) immediately -- no response comes back.
        // Just kill the child process directly.
        let _ = self.child.kill();
        let _ = self.child.wait();
        Ok(())
    }
}

// ─── WebKitPage ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct WebKitPage {
    client: Arc<IpcClient>,
    view_id: u64,
}

impl WebKitPage {
    fn vid(&self) -> u64 { self.view_id }

    fn ok(&self, r: IpcResponse) -> Result<(), String> {
        match r { IpcResponse::Ok => Ok(()), IpcResponse::Error(e) => Err(e), _ => Ok(()) }
    }

    pub async fn goto(&self, url: &str) -> Result<(), String> {
        let r = self.client.send_str_vid(Op::Navigate, url, self.vid()).await?;
        self.ok(r)?;
        let r2 = self.client.send_vid(Op::WaitNav, self.vid()).await?;
        self.ok(r2)
    }

    pub async fn wait_for_navigation(&self) -> Result<(), String> {
        let r = self.client.send_vid(Op::WaitNav, self.vid()).await?;
        self.ok(r)
    }

    pub async fn reload(&self) -> Result<(), String> {
        let r = self.client.send_vid(Op::Reload, self.vid()).await?;
        self.ok(r)
    }

    pub async fn go_back(&self) -> Result<(), String> {
        let r = self.client.send_vid(Op::GoBack, self.vid()).await?;
        self.ok(r)?;
        // Wait for navigation to complete via nav delegate
        let r2 = self.client.send_vid(Op::WaitNav, self.vid()).await?;
        self.ok(r2)
    }

    pub async fn go_forward(&self) -> Result<(), String> {
        let r = self.client.send_vid(Op::GoForward, self.vid()).await?;
        self.ok(r)?;
        let r2 = self.client.send_vid(Op::WaitNav, self.vid()).await?;
        self.ok(r2)
    }

    pub async fn url(&self) -> Result<Option<String>, String> {
        let r = self.client.send_vid(Op::GetUrl, self.vid()).await?;
        match r {
            IpcResponse::Value(v) => Ok(v.as_str().map(|s| s.to_string())),
            IpcResponse::Error(e) => Err(e),
            _ => Ok(None),
        }
    }

    pub async fn title(&self) -> Result<Option<String>, String> {
        let r = self.client.send_vid(Op::GetTitle, self.vid()).await?;
        match r {
            IpcResponse::Value(v) => Ok(v.as_str().map(|s| s.to_string())),
            IpcResponse::Error(e) => Err(e),
            _ => Ok(None),
        }
    }

    pub async fn evaluate(&self, expression: &str) -> Result<Option<serde_json::Value>, String> {
        let r = self.client.send_str_vid(Op::Evaluate, expression, self.vid()).await?;
        match r {
            IpcResponse::Value(v) => if v.is_null() { Ok(None) } else { Ok(Some(v)) },
            IpcResponse::Error(e) => Err(e),
            _ => Ok(None),
        }
    }

    pub async fn find_element(&self, selector: &str) -> Result<AnyElement, String> {
        let js = format!(
            r#"(function(){{var e=document.querySelector('{}');if(!e)return null;if(!window.__wr)window.__wr={{}};var id=Object.keys(window.__wr).length+1;window.__wr[id]=e;return id}})()"#,
            selector.replace('\\', "\\\\").replace('\'', "\\'")
        );
        let r = self.evaluate(&js).await?;
        let ref_id = r.and_then(|v| v.as_u64()).ok_or_else(|| format!("'{selector}' not found"))?;
        Ok(AnyElement::WebKit(WebKitElement { client: self.client.clone(), view_id: self.view_id, ref_id }))
    }

    pub async fn evaluate_to_element(&self, js: &str) -> Result<AnyElement, String> {
        let escaped = js.replace('\\', "\\\\").replace('\'', "\\'");
        let wrap = format!(
            r#"(function(){{var e=({escaped});if(!e)return null;if(!window.__wr)window.__wr={{}};var id=Object.keys(window.__wr).length+1;window.__wr[id]=e;return id}})()"#
        );
        let r = self.evaluate(&wrap).await?;
        let ref_id = r.and_then(|v| v.as_u64()).ok_or("JS did not return a DOM element")?;
        Ok(AnyElement::WebKit(WebKitElement { client: self.client.clone(), view_id: self.view_id, ref_id }))
    }

    pub async fn content(&self) -> Result<String, String> {
        let r = self.evaluate("document.documentElement.outerHTML").await?;
        Ok(r.and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default())
    }

    pub async fn set_content(&self, html: &str) -> Result<(), String> {
        let mut p = Vec::new();
        p.extend_from_slice(&self.vid().to_le_bytes());
        ipc::str_encode(&mut p, html);
        ipc::str_encode(&mut p, "about:blank");
        let r = self.client.send(ipc::Op::LoadHtml, &p).await?;
        self.ok(r)
    }

    pub async fn screenshot(&self, opts: ScreenshotOpts) -> Result<Vec<u8>, String> {
        // Send format + quality as payload: u8 format (0=png, 1=jpeg, 2=webp) + u8 quality + u64 vid
        let mut p = Vec::new();
        let fmt_byte: u8 = match opts.format {
            ImageFormat::Jpeg => 1,
            ImageFormat::Webp => 2,
            _ => 0,
        };
        p.push(fmt_byte);
        p.push(opts.quality.unwrap_or(80) as u8);
        p.extend_from_slice(&self.vid().to_le_bytes());
        let r = self.client.send(Op::Screenshot, &p).await?;
        match r {
            IpcResponse::Binary(d) => Ok(d),
            IpcResponse::Error(e) => Err(e),
            _ => Err("no data".into()),
        }
    }

    pub async fn screenshot_element(&self, sel: &str, _fmt: ImageFormat) -> Result<Vec<u8>, String> {
        // Scroll element into view, then take full screenshot
        // WKWebView doesn't support clipped screenshots natively
        let esc = sel.replace('\'', "\\'");
        let _ = self.evaluate(&format!(
            "document.querySelector('{esc}')?.scrollIntoView({{block:'center'}})"
        )).await;
        self.screenshot(ScreenshotOpts::default()).await
    }

    pub async fn pdf(&self, _landscape: bool, _print_background: bool) -> Result<Vec<u8>, String> {
        // WKWebView has createPDF but it requires a new IPC op.
        // For now, generate via JS print-to-PDF workaround is not possible.
        // Use evaluate to check if we can at least return the HTML for external conversion.
        Err("PDF generation requires CDP backend (cdp-ws, cdp-pipe, or cdp-raw)".into())
    }

    pub async fn set_file_input(&self, selector: &str, paths: &[String]) -> Result<(), String> {
        if paths.is_empty() {
            return Err("No file paths provided".into());
        }
        // WebKit uses a custom IPC op that reads the file in ObjC and injects via DataTransfer API
        let mut p = Vec::new();
        ipc::str_encode(&mut p, selector);
        ipc::str_encode(&mut p, &paths[0]); // First file only (multi-file needs multiple calls)
        p.extend_from_slice(&self.view_id.to_le_bytes());
        let r = self.client.send(ipc::Op::SetFileInput, &p).await?;
        self.ok(r)
    }

    pub async fn accessibility_tree(&self) -> Result<Vec<AxNodeData>, String> {
        let js = r#"(function(){function w(e,p,n){var id='n'+n.length;var role=e.getAttribute('role')||e.tagName?.toLowerCase()||'generic';var name=e.getAttribute('aria-label')||e.getAttribute('alt')||e.getAttribute('title')||(e.tagName==='INPUT'?e.getAttribute('placeholder'):'')||(e.childNodes.length===1&&e.childNodes[0].nodeType===3?e.childNodes[0].textContent?.trim():'')||'';var props=[];if(e.getAttribute('aria-level'))props.push({name:'level',value:e.getAttribute('aria-level')});if(e.disabled)props.push({name:'disabled',value:true});if(e.required)props.push({name:'required',value:true});if(e.href)props.push({name:'url',value:e.href});n.push({nodeId:id,parentId:p,role:role,name:name,description:'',properties:props,ignored:false});for(var c of e.children)w(c,id,n)}var n=[];w(document.body||document.documentElement,null,n);return JSON.stringify(n)})()"#;
        let r = self.evaluate(js).await?;
        let json_str = r.and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or("[]".into());
        let raw: Vec<serde_json::Value> = serde_json::from_str(&json_str).map_err(|e| format!("{e}"))?;
        Ok(raw.iter().map(|n| AxNodeData {
            node_id: n.get("nodeId").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            parent_id: n.get("parentId").and_then(|v| v.as_str()).map(|s| s.to_string()),
            backend_dom_node_id: None,
            ignored: false,
            role: n.get("role").and_then(|v| v.as_str()).map(|s| s.to_string()),
            name: n.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_string()),
            description: None,
            properties: n.get("properties").and_then(|p| p.as_array()).map(|ps| {
                ps.iter().map(|p| AxProperty {
                    name: p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    value: p.get("value").cloned(),
                }).collect()
            }).unwrap_or_default(),
        }).collect())
    }

    pub async fn click_at(&self, x: f64, y: f64) -> Result<(), String> {
        self.click_at_opts(x, y, "left", 1).await
    }

    pub async fn click_at_opts(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<(), String> {
        let btn: u8 = match button { "right" => 1, "middle" => 2, _ => 0 };
        // NSEvent clickCount must increment per click for dblclick to fire.
        // e.g. click_count=2: first pair has clickCount=1, second has clickCount=2.
        for i in 1..=click_count {
            self.send_mouse_event(1, btn, i, x, y).await?; // down
            self.send_mouse_event(2, btn, i, x, y).await?; // up
        }
        Ok(())
    }

    pub async fn move_mouse(&self, x: f64, y: f64) -> Result<(), String> {
        // WKWebView's mouseMoved: doesn't propagate to DOM in offscreen windows.
        // Use WKWebView.evaluateJavaScript to dispatch the event directly in the
        // web content process, same approach as Playwright's webkit backend.
        let js = format!(
            "(function(){{var e=document.elementFromPoint({x},{y});\
            if(e)e.dispatchEvent(new MouseEvent('mousemove',{{clientX:{x},clientY:{y},bubbles:true,view:window}}))}})()");
        self.evaluate(&js).await?;
        Ok(())
    }

    pub async fn move_mouse_smooth(&self, from_x: f64, from_y: f64, to_x: f64, to_y: f64, steps: u32) -> Result<(), String> {
        let steps = steps.max(1);
        // Batch all moves into one JS evaluate for performance
        let mut js = String::with_capacity(steps as usize * 120 + 20);
        js.push_str("(function(){");
        for i in 0..=steps {
            let t = i as f64 / steps as f64;
            let ease = t * t * (3.0 - 2.0 * t);
            let x = from_x + (to_x - from_x) * ease;
            let y = from_y + (to_y - from_y) * ease;
            use std::fmt::Write;
            let _ = write!(js,
                "var e=document.elementFromPoint({x},{y});\
                if(e)e.dispatchEvent(new MouseEvent('mousemove',{{clientX:{x},clientY:{y},bubbles:true,view:window}}));"
            );
        }
        js.push_str("})()");
        self.evaluate(&js).await?;
        Ok(())
    }

    pub async fn click_and_drag(&self, from: (f64, f64), to: (f64, f64)) -> Result<(), String> {
        self.send_mouse_event(1, 0, 1, from.0, from.1).await?; // down
        let steps = 10u32;
        for i in 1..=steps {
            let t = i as f64 / steps as f64;
            let ease = t * t * (3.0 - 2.0 * t);
            let x = from.0 + (to.0 - from.0) * ease;
            let y = from.1 + (to.1 - from.1) * ease;
            self.send_mouse_event(0, 0, 0, x, y).await?; // move
        }
        self.send_mouse_event(2, 0, 1, to.0, to.1).await // up
    }

    /// Send a native mouse event via IPC.
    /// mouse_type: 0=move, 1=down, 2=up
    /// button: 0=left, 1=right, 2=middle
    async fn send_mouse_event(&self, mouse_type: u8, button: u8, click_count: u32, x: f64, y: f64) -> Result<(), String> {
        let mut p = Vec::with_capacity(27);
        p.push(mouse_type);
        p.push(button);
        p.extend_from_slice(&click_count.to_le_bytes());
        p.extend_from_slice(&x.to_le_bytes());
        p.extend_from_slice(&y.to_le_bytes());
        p.extend_from_slice(&self.vid().to_le_bytes());
        let r = self.client.send(ipc::Op::MouseEvent, &p).await?;
        self.ok(r)
    }

    pub async fn type_str(&self, text: &str) -> Result<(), String> {
        let mut p = Vec::new();
        ipc::str_encode(&mut p, text);
        p.extend_from_slice(&self.vid().to_le_bytes());
        let r = self.client.send(Op::Type, &p).await?;
        self.ok(r)
    }

    pub async fn press_key(&self, key: &str) -> Result<(), String> {
        let mut p = Vec::new();
        ipc::str_encode(&mut p, key);
        p.extend_from_slice(&self.vid().to_le_bytes());
        let r = self.client.send(Op::PressKey, &p).await?;
        self.ok(r)
    }

    pub async fn get_cookies(&self) -> Result<Vec<CookieData>, String> {
        let mut p = Vec::new();
        p.extend_from_slice(&self.vid().to_le_bytes());
        let r = self.client.send(ipc::Op::GetCookies, &p).await?;
        match r {
            ipc::IpcResponse::Value(v) => {
                // The IPC reader already parses the JSON string into a Value.
                // Deserialize directly from the parsed Value.
                Ok(serde_json::from_value(v).unwrap_or_default())
            }
            ipc::IpcResponse::Error(e) => Err(e),
            _ => Err("unexpected response".into()),
        }
    }

    pub async fn set_cookie(&self, c: CookieData) -> Result<(), String> {
        let mut p = Vec::new();
        p.extend_from_slice(&self.vid().to_le_bytes());
        ipc::str_encode(&mut p, &c.name);
        ipc::str_encode(&mut p, &c.value);
        ipc::str_encode(&mut p, &c.domain);
        ipc::str_encode(&mut p, &c.path);
        p.push(u8::from(c.secure));
        p.push(u8::from(c.http_only));
        let expires = c.expires.unwrap_or(-1.0);
        p.extend_from_slice(&expires.to_le_bytes());
        let r = self.client.send(ipc::Op::SetCookie, &p).await?;
        self.ok(r)
    }

    pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<(), String> {
        let mut p = Vec::new();
        p.extend_from_slice(&self.vid().to_le_bytes());
        ipc::str_encode(&mut p, name);
        ipc::str_encode(&mut p, domain.unwrap_or(""));
        let r = self.client.send(ipc::Op::DeleteCookie, &p).await?;
        self.ok(r)
    }

    pub async fn clear_cookies(&self) -> Result<(), String> {
        let mut p = Vec::new();
        p.extend_from_slice(&self.vid().to_le_bytes());
        let r = self.client.send(ipc::Op::ClearCookies, &p).await?;
        self.ok(r)
    }

    pub async fn emulate_viewport(&self, config: &crate::options::ViewportConfig) -> Result<(), String> {
        // Native resize + scale via IPC -- sets window backingScaleFactor,
        // resizes NSWindow and WKWebView frame. Affects actual rendering.
        let mut p = Vec::new();
        p.extend_from_slice(&(config.width as f64).to_le_bytes());
        p.extend_from_slice(&(config.height as f64).to_le_bytes());
        p.extend_from_slice(&config.device_scale_factor.to_le_bytes());
        p.extend_from_slice(&self.vid().to_le_bytes());
        let r = self.client.send(ipc::Op::SetViewport, &p).await?;
        self.ok(r)
    }

    pub async fn set_user_agent(&self, ua: &str) -> Result<(), String> {
        let mut p = Vec::new();
        ipc::str_encode(&mut p, ua);
        p.extend_from_slice(&self.vid().to_le_bytes());
        let r = self.client.send(Op::SetUserAgent, &p).await?;
        self.ok(r)
    }

    pub async fn set_geolocation(&self, lat: f64, lng: f64, acc: f64) -> Result<(), String> {
        let js = format!(
            "(function(){{var pos={{coords:{{latitude:{lat},longitude:{lng},accuracy:{acc},altitude:null,altitudeAccuracy:null,heading:null,speed:null}},timestamp:Date.now()}};navigator.geolocation.getCurrentPosition=function(s){{s(pos)}};navigator.geolocation.watchPosition=function(s){{s(pos);return 0}}}})()"
        );
        self.evaluate(&js).await?;
        Ok(())
    }

    pub async fn set_network_state(&self, offline: bool, _lat: f64, _dl: f64, _ul: f64) -> Result<(), String> {
        // Can only emulate offline/online via navigator.onLine override
        // Throttling not possible without native NSURLProtocol interception
        let js = format!(
            "Object.defineProperty(navigator,'onLine',{{get:function(){{return {}}},configurable:true}})",
            if offline { "false" } else { "true" }
        );
        self.evaluate(&js).await?;
        Ok(())
    }

    pub async fn set_locale(&self, locale: &str) -> Result<(), String> {
        let mut p = Vec::new();
        p.extend_from_slice(&self.vid().to_le_bytes());
        ipc::str_encode(&mut p, locale);
        let r = self.client.send(ipc::Op::SetLocale, &p).await?;
        self.ok(r)
    }

    pub async fn set_timezone(&self, timezone_id: &str) -> Result<(), String> {
        let mut p = Vec::new();
        p.extend_from_slice(&self.vid().to_le_bytes());
        ipc::str_encode(&mut p, timezone_id);
        let r = self.client.send(ipc::Op::SetTimezone, &p).await?;
        self.ok(r)
    }

    pub async fn emulate_media(&self, opts: &crate::options::EmulateMediaOptions) -> Result<(), String> {
        let mut p = Vec::new();
        p.extend_from_slice(&self.vid().to_le_bytes());
        ipc::str_encode(&mut p, opts.color_scheme.as_deref().unwrap_or(""));
        ipc::str_encode(&mut p, opts.reduced_motion.as_deref().unwrap_or(""));
        ipc::str_encode(&mut p, opts.forced_colors.as_deref().unwrap_or(""));
        ipc::str_encode(&mut p, opts.media.as_deref().unwrap_or(""));
        let r = self.client.send(ipc::Op::EmulateMedia, &p).await?;
        self.ok(r)
    }

    pub async fn set_javascript_enabled(&self, _enabled: bool) -> Result<(), String> {
        // WKWebView doesn't support disabling JS at runtime
        Ok(())
    }

    pub async fn set_extra_http_headers(&self, _headers: &rustc_hash::FxHashMap<String, String>) -> Result<(), String> {
        // Would require native WKURLSchemeHandler -- not trivially possible
        Ok(())
    }

    pub async fn grant_permissions(&self, _permissions: &[String], _origin: Option<&str>) -> Result<(), String> {
        Ok(())
    }

    pub async fn reset_permissions(&self) -> Result<(), String> {
        Ok(())
    }

    pub async fn set_focus_emulation_enabled(&self, _enabled: bool) -> Result<(), String> {
        Ok(())
    }

    pub async fn start_tracing(&self) -> Result<(), String> {
        // Mark the start time for performance measurement
        self.evaluate("window.__fd_trace_start = performance.now()").await?;
        Ok(())
    }

    pub async fn stop_tracing(&self) -> Result<(), String> {
        self.evaluate("window.__fd_trace_end = performance.now()").await?;
        Ok(())
    }

    pub async fn metrics(&self) -> Result<Vec<MetricData>, String> {
        let js = r#"(function(){var p=performance.getEntriesByType('navigation')[0];if(!p)return'[]';return JSON.stringify([{name:'DOMContentLoaded',value:p.domContentLoadedEventEnd},{name:'Load',value:p.loadEventEnd},{name:'TTFB',value:p.responseStart}])})()"#;
        let r = self.evaluate(js).await?;
        let s = r.and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or("[]".into());
        Ok(serde_json::from_str(&s).unwrap_or_default())
    }

    pub async fn resolve_backend_node(&self, _id: i64, ref_id: &str) -> Result<AnyElement, String> {
        self.find_element(&format!("[data-cref='{ref_id}']")).await
    }

    pub fn attach_listeners(&self, console_log: Arc<RwLock<Vec<ConsoleMsg>>>, net_log: Arc<RwLock<Vec<NetRequest>>>, dialog_log: Arc<RwLock<Vec<crate::state::DialogEvent>>>) {
        // All events pushed instantly by ObjC WKScriptMessageHandler → binary IPC frame.
        // No polling, no JS evaluation. Same efficient pattern for console, dialog, and network.
        // Drain task runs at 10ms to move events from IPC shared vecs to session-scoped async logs.
        let client = self.client.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;

                // Drain console events
                {
                    let msgs: Vec<(String, String)> = {
                        let mut log = client.console_log.lock().unwrap();
                        if log.is_empty() { Vec::new() } else { std::mem::take(&mut *log) }
                    };
                    if !msgs.is_empty() {
                        let mut dest = console_log.write().await;
                        for (level, text) in msgs {
                            dest.push(ConsoleMsg { level, text });
                        }
                    }
                }

                // Drain dialog events
                {
                    let evts: Vec<(String, String, String)> = {
                        let mut log = client.dialog_log.lock().unwrap();
                        if log.is_empty() { Vec::new() } else { std::mem::take(&mut *log) }
                    };
                    if !evts.is_empty() {
                        let mut dest = dialog_log.write().await;
                        for (dtype, message, action) in evts {
                            dest.push(crate::state::DialogEvent { dialog_type: dtype, message, action });
                        }
                    }
                }

                // Drain network events
                {
                    let evts: Vec<(String, String, String, String)> = {
                        let mut log = client.network_log.lock().unwrap();
                        if log.is_empty() { Vec::new() } else { std::mem::take(&mut *log) }
                    };
                    if !evts.is_empty() {
                        let mut dest = net_log.write().await;
                        for (id, method, url, resource_type) in evts {
                            dest.push(NetRequest { id, method, url, resource_type, status: None, mime_type: None });
                        }
                    }
                }
            }
        });
    }
}

// ─── WebKitElement ──────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct WebKitElement {
    client: Arc<IpcClient>,
    view_id: u64,
    ref_id: u64,
}

impl WebKitElement {
    fn el(&self) -> String { format!("window.__wr[{}]", self.ref_id) }

    async fn eval(&self, js: &str) -> Result<(), String> {
        let mut p = Vec::new();
        ipc::str_encode(&mut p, js);
        p.extend_from_slice(&self.view_id.to_le_bytes());
        let _ = self.client.send(Op::Evaluate, &p).await?;
        Ok(())
    }

    pub async fn click(&self) -> Result<(), String> {
        // Scroll into view first
        let _ = self.scroll_into_view().await;
        // Get element center coordinates, then use native NSEvent click (OP_CLICK)
        // instead of JS .click() which doesn't trigger native focus behavior.
        let js = format!(
            "(function(){{var e={};var r=e.getBoundingClientRect();return r.left+r.width/2+','+( r.top+r.height/2)}})()",
            self.el()
        );
        let mut p = Vec::new();
        ipc::str_encode(&mut p, &js);
        p.extend_from_slice(&self.view_id.to_le_bytes());
        let r = self.client.send(ipc::Op::Evaluate, &p).await?;
        match r {
            IpcResponse::Value(v) => {
                let coords = v.as_str().unwrap_or("0,0");
                let parts: Vec<&str> = coords.split(',').collect();
                let x: f64 = parts.get(0).and_then(|s| s.parse().ok()).unwrap_or(0.0);
                let y: f64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0.0);
                // Native click via NSEvent
                let mut click_p = Vec::new();
                click_p.extend_from_slice(&x.to_le_bytes());
                click_p.extend_from_slice(&y.to_le_bytes());
                click_p.extend_from_slice(&self.view_id.to_le_bytes());
                let r2 = self.client.send(ipc::Op::Click, &click_p).await?;
                match r2 { IpcResponse::Error(e) => Err(e), _ => Ok(()) }
            }
            IpcResponse::Error(e) => Err(e),
            // Fallback to JS click if coordinate extraction fails
            _ => self.eval(&format!("{}.click()", self.el())).await,
        }
    }

    pub async fn hover(&self) -> Result<(), String> {
        self.eval(&format!("{}.dispatchEvent(new MouseEvent('mouseenter',{{bubbles:true}}))", self.el())).await
    }

    pub async fn type_str(&self, text: &str) -> Result<(), String> {
        let esc = text.replace('\\', "\\\\").replace('\'', "\\'");
        self.eval(&format!("(function(){{var e={};e.focus();e.value+='{esc}';e.dispatchEvent(new Event('input',{{bubbles:true}}))}})()", self.el())).await
    }

    pub async fn call_js_fn(&self, func: &str) -> Result<(), String> {
        self.eval(&format!("({}).call({})", func, self.el())).await
    }

    pub async fn call_js_fn_value(&self, func: &str) -> Result<Option<serde_json::Value>, String> {
        let js = format!("JSON.stringify(({}).call({}))", func, self.el());
        let mut p = Vec::new();
        ipc::str_encode(&mut p, &js);
        p.extend_from_slice(&self.view_id.to_le_bytes());
        let r = self.client.send(ipc::Op::Evaluate, &p).await?;
        match r {
            ipc::IpcResponse::Value(serde_json::Value::String(s)) => {
                Ok(serde_json::from_str(&s).ok())
            }
            ipc::IpcResponse::Value(v) => Ok(Some(v)),
            ipc::IpcResponse::Error(e) => Err(e),
            _ => Ok(None),
        }
    }

    pub async fn scroll_into_view(&self) -> Result<(), String> {
        self.eval(&format!("{}.scrollIntoView({{behavior:'instant',block:'center'}})", self.el())).await
    }

    pub async fn screenshot(&self, fmt: ImageFormat) -> Result<Vec<u8>, String> {
        // Must match page screenshot payload: u8 format + u8 quality + u64 vid
        let mut p = Vec::new();
        let fmt_byte: u8 = match fmt {
            ImageFormat::Jpeg => 1,
            ImageFormat::Webp => 2,
            _ => 0,
        };
        p.push(fmt_byte);
        p.push(80); // default quality
        p.extend_from_slice(&self.view_id.to_le_bytes());
        let r = self.client.send(Op::Screenshot, &p).await?;
        match r { IpcResponse::Binary(d) => Ok(d), IpcResponse::Error(e) => Err(e), _ => Err("no data".into()) }
    }
}
