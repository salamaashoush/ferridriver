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
            IpcResponse::ViewCreated(id) => Ok(AnyPage::WebKit(WebKitPage {
                client: self.client.clone(), view_id: id,
            })),
            IpcResponse::Error(e) => Err(e),
            _ => Err("unexpected".into()),
        }
    }

    pub async fn new_page_isolated(&self, url: &str) -> Result<AnyPage, String> {
        self.new_page(url).await
    }

    pub async fn close(&mut self) -> Result<(), String> {
        let _ = self.client.send_empty(Op::Shutdown).await;
        let _ = self.child.kill();
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
        // Wait for load like Bun
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

    pub async fn content(&self) -> Result<String, String> {
        let r = self.evaluate("document.documentElement.outerHTML").await?;
        Ok(r.and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default())
    }

    pub async fn set_content(&self, html: &str) -> Result<(), String> {
        let esc = html.replace('\\', "\\\\").replace('`', "\\`");
        self.evaluate(&format!("document.documentElement.innerHTML=`{esc}`")).await?;
        Ok(())
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
        let mut p = Vec::new();
        p.extend_from_slice(&x.to_le_bytes());
        p.extend_from_slice(&y.to_le_bytes());
        p.extend_from_slice(&self.vid().to_le_bytes());
        let r = self.client.send(Op::Click, &p).await?;
        self.ok(r)
    }

    pub async fn click_and_drag(&self, from: (f64, f64), to: (f64, f64)) -> Result<(), String> {
        let js = format!(
            "(function(){{var f=document.elementFromPoint({},{});if(f){{f.dispatchEvent(new MouseEvent('mousedown',{{clientX:{},clientY:{},bubbles:true}}));f.dispatchEvent(new MouseEvent('mousemove',{{clientX:{},clientY:{},bubbles:true}}));f.dispatchEvent(new MouseEvent('mouseup',{{clientX:{},clientY:{},bubbles:true}}))}}}})()",
            from.0, from.1, from.0, from.1, to.0, to.1, to.0, to.1
        );
        self.evaluate(&js).await?;
        Ok(())
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
        let js = r#"JSON.stringify(document.cookie.split(';').map(function(c){var p=c.trim().split('=');return{name:p[0],value:p.slice(1).join('='),domain:location.hostname,path:'/',secure:false,http_only:false}}))"#;
        let r = self.evaluate(js).await?;
        let s = r.and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or("[]".into());
        Ok(serde_json::from_str(&s).unwrap_or_default())
    }

    pub async fn set_cookie(&self, c: CookieData) -> Result<(), String> {
        let mut parts = vec![format!("{}={}", c.name, c.value)];
        if !c.domain.is_empty() { parts.push(format!("domain={}", c.domain)); }
        if !c.path.is_empty() { parts.push(format!("path={}", c.path)); }
        if c.secure { parts.push("secure".into()); }
        self.evaluate(&format!("document.cookie='{}'", parts.join("; ").replace('\'', "\\'"))).await?;
        Ok(())
    }

    pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<(), String> {
        let mut cookie = format!("{}=;expires=Thu,01 Jan 1970 00:00:00 UTC;path=/", name);
        if let Some(d) = domain {
            cookie.push_str(&format!(";domain={d}"));
        }
        self.evaluate(&format!("document.cookie='{}'", cookie.replace('\'', "\\'"))).await?;
        Ok(())
    }

    pub async fn clear_cookies(&self) -> Result<(), String> {
        self.evaluate("document.cookie.split(';').forEach(function(c){document.cookie=c.trim().split('=')[0]+'=;expires=Thu,01 Jan 1970 00:00:00 UTC;path=/'})").await?;
        Ok(())
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

    pub async fn scroll_into_view(&self) -> Result<(), String> {
        self.eval(&format!("{}.scrollIntoView({{behavior:'instant',block:'center'}})", self.el())).await
    }

    pub async fn screenshot(&self, _fmt: ImageFormat) -> Result<Vec<u8>, String> {
        let mut p = Vec::new();
        p.extend_from_slice(&self.view_id.to_le_bytes());
        let r = self.client.send(Op::Screenshot, &p).await?;
        match r { IpcResponse::Binary(d) => Ok(d), IpcResponse::Error(e) => Err(e), _ => Err("no data".into()) }
    }
}
