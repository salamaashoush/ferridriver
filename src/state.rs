//! Browser state management with session-scoped isolation.
//!
//! Design principles:
//! - No global "active page" — every tool call specifies its session
//! - Sessions map to Chrome BrowserContexts (isolated cookies, storage, websockets)
//! - No races possible: there is no shared mutable selection state
//! - Single-threaded tokio runtime: Mutex is never actually contended

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::{
    EventRequestWillBeSent, EventResponseReceived,
};
use chromiumoxide::cdp::js_protocol::runtime::EventConsoleApiCalled;
use chromiumoxide::Page;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// A collected console message.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConsoleMsg {
    pub level: String,
    pub text: String,
}

/// A collected network request.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NetRequest {
    pub id: String,
    pub method: String,
    pub url: String,
    pub resource_type: String,
    pub status: Option<i64>,
    pub mime_type: Option<String>,
}

/// A single isolated session backed by a Chrome BrowserContext.
pub struct Session {
    pub pages: Vec<Page>,
    pub active_page_idx: usize,
    pub ref_map: HashMap<String, i64>,
    /// Console messages collected from CDP events.
    pub console_log: Arc<RwLock<Vec<ConsoleMsg>>>,
    /// Network requests collected from CDP events.
    pub network_log: Arc<RwLock<Vec<NetRequest>>>,
}

impl Session {
    fn new() -> Self {
        Self {
            pages: Vec::new(),
            active_page_idx: 0,
            ref_map: HashMap::new(),
            console_log: Arc::new(RwLock::new(Vec::new())),
            network_log: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn active_page(&self) -> Option<&Page> {
        self.pages.get(self.active_page_idx)
    }
}

/// All browser state.
pub struct BrowserState {
    browser: Option<Browser>,
    handler_handle: Option<tokio::task::JoinHandle<()>>,
    sessions: HashMap<String, Session>,
    chromium_path: String,
    /// Connection mode
    connect_mode: ConnectMode,
}

#[derive(Clone)]
pub enum ConnectMode {
    /// Launch a new browser (default)
    Launch,
    /// Connect to browser at explicit ws:// or http:// URL
    ConnectUrl(String),
    /// Auto-connect to running Chrome by reading DevToolsActivePort file
    /// from Chrome's user data directory (like Chrome DevTools MCP --autoConnect)
    AutoConnect {
        channel: String,
        user_data_dir: Option<String>,
    },
}

impl BrowserState {
    pub fn new(connect_mode: ConnectMode) -> Self {
        let chromium_path =
            std::env::var("CHROMIUM_PATH").unwrap_or_else(|_| detect_chromium());
        Self {
            browser: None,
            handler_handle: None,
            sessions: HashMap::new(),
            chromium_path,
            connect_mode,
        }
    }

    /// Ensure browser is launched or connected.
    pub async fn ensure_browser(&mut self) -> Result<(), String> {
        if self.browser.is_some() {
            return Ok(());
        }

        let (browser, handler) = match &self.connect_mode {
            ConnectMode::ConnectUrl(url) => {
                let ws_url = if url.starts_with("ws://") || url.starts_with("wss://") {
                    url.clone()
                } else {
                    // HTTP URL — discover WS endpoint via /json/version
                    discover_ws_from_http(url).await?
                };
                Browser::connect(&ws_url)
                    .await
                    .map_err(|e| format!("Connect to {ws_url} failed: {e}"))?
            }
            ConnectMode::AutoConnect { channel, user_data_dir } => {
                let ws_url = discover_chrome_ws(channel, user_data_dir.as_deref())?;
                eprintln!("Auto-connecting to Chrome at {ws_url}");
                Browser::connect(&ws_url)
                    .await
                    .map_err(|e| format!(
                        "Auto-connect failed: {e}. Ensure Chrome is running and remote debugging \
                         is enabled at chrome://inspect/#remote-debugging"
                    ))?
            }
            ConnectMode::Launch => {
                let user_data_dir =
                    std::env::temp_dir().join(format!("chromey-mcp-{}", std::process::id()));

                let config = BrowserConfig::builder()
                    .chrome_executable(&self.chromium_path)
                    .user_data_dir(user_data_dir)
                    .no_sandbox()
                    .arg("--disable-gpu")
                    .arg("--disable-dev-shm-usage")
                    .arg("--disable-extensions")
                    .arg("--disable-background-networking")
                    .arg("--disable-background-timer-throttling")
                    .arg("--disable-backgrounding-occluded-windows")
                    .arg("--disable-renderer-backgrounding")
                    .arg("--disable-sync")
                    .arg("--disable-translate")
                    .arg("--disable-default-apps")
                    .arg("--disable-component-update")
                    .arg("--no-first-run")
                    .arg("--no-default-browser-check")
                    .viewport(None)
                    .build()
                    .map_err(|e| format!("Browser config error: {e}"))?;

                Browser::launch(config)
                    .await
                    .map_err(|e| format!("Browser launch failed: {e}"))?
            }
        };

        let mut handler = handler;
        let handle = tokio::spawn(async move {
            while let Some(h) = handler.next().await {
                if h.is_err() {
                    break;
                }
            }
        });

        self.browser = Some(browser);
        self.handler_handle = Some(handle);

        // Discover existing pages or create one
        let existing_pages = self.browser()?.pages().await.unwrap_or_default();
        let pages = if existing_pages.is_empty() {
            let page = self.browser()?.new_page("about:blank").await
                .map_err(|e| format!("Initial page failed: {e}"))?;
            vec![page]
        } else {
            existing_pages
        };
        let sess = self.session_mut("default");
        for page in pages {
            Self::attach_listeners(&page, &sess.console_log, &sess.network_log);
            sess.pages.push(page);
        }

        Ok(())
    }

    fn browser(&self) -> Result<&Browser, String> {
        self.browser.as_ref().ok_or_else(|| "Browser not launched".into())
    }

    fn session_mut(&mut self, name: &str) -> &mut Session {
        self.sessions
            .entry(name.to_string())
            .or_insert_with(Session::new)
    }

    pub fn session(&self, name: &str) -> Result<&Session, String> {
        self.sessions
            .get(name)
            .ok_or_else(|| format!("Session '{name}' not found. Use new_page with session='{name}' to create it."))
    }

    pub fn session_mut_checked(&mut self, name: &str) -> Result<&mut Session, String> {
        self.sessions
            .get_mut(name)
            .ok_or_else(|| format!("Session '{name}' not found."))
    }

    /// Attach console and network event listeners to a page.
    fn attach_listeners(
        page: &Page,
        console_log: &Arc<RwLock<Vec<ConsoleMsg>>>,
        network_log: &Arc<RwLock<Vec<NetRequest>>>,
    ) {
        // Console listener
        let cl = console_log.clone();
        let page_clone = page.clone();
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
        let page_clone = page.clone();
        tokio::spawn(async move {
            if let Ok(mut stream) = page_clone.event_listener::<EventRequestWillBeSent>().await {
                while let Some(ev) = stream.next().await {
                    nl.write().await.push(NetRequest {
                        id: ev.request_id.inner().clone(),
                        method: ev.request.method.clone(),
                        url: ev.request.url.clone(),
                        resource_type: ev.r#type.as_ref().map(|t| format!("{t:?}")).unwrap_or_default(),
                        status: None,
                        mime_type: None,
                    });
                }
            }
        });

        // Network response listener (updates status)
        let nl2 = network_log.clone();
        let page_clone = page.clone();
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
    }

    /// Open a new page in a session.
    pub async fn open_page(&mut self, session: &str, url: &str) -> Result<usize, String> {
        self.ensure_browser().await?;
        let browser = self.browser()?;

        let page = if session == "default" {
            browser
                .new_page(url)
                .await
                .map_err(|e| format!("New page failed: {e}"))?
        } else {
            use chromiumoxide::cdp::browser_protocol::target::{
                CreateBrowserContextParams, CreateTargetParams,
            };
            let ctx_id = {
                let params = CreateBrowserContextParams::builder().build();
                let result = browser
                    .execute(params)
                    .await
                    .map_err(|e| format!("Create browser context failed: {e}"))?;
                result.result.browser_context_id
            };
            let mut create_params = CreateTargetParams::new(url);
            create_params.browser_context_id = Some(ctx_id);
            browser
                .new_page(create_params)
                .await
                .map_err(|e| format!("New page in context failed: {e}"))?
        };

        if url != "about:blank" {
            let _ = page.wait_for_navigation().await;
        }

        let sess = self.session_mut(session);
        Self::attach_listeners(&page, &sess.console_log, &sess.network_log);
        let idx = sess.pages.len();
        sess.pages.push(page);
        sess.active_page_idx = idx;

        Ok(idx)
    }

    pub fn active_page(&self, session: &str) -> Result<&Page, String> {
        let sess = self.session(session)?;
        sess.active_page()
            .ok_or_else(|| format!("No pages in session '{session}'"))
    }

    pub fn select_page(&mut self, session: &str, page_idx: usize) -> Result<(), String> {
        let sess = self.session_mut_checked(session)?;
        if page_idx >= sess.pages.len() {
            return Err(format!(
                "Page index {page_idx} out of range (session '{session}' has {} pages)",
                sess.pages.len()
            ));
        }
        sess.active_page_idx = page_idx;
        Ok(())
    }

    pub fn close_page(&mut self, session: &str, page_idx: usize) -> Result<(), String> {
        let sess = self.session_mut_checked(session)?;
        if sess.pages.len() <= 1 {
            return Err("Cannot close the last page in a session".into());
        }
        if page_idx >= sess.pages.len() {
            return Err(format!("Page index {page_idx} out of range"));
        }
        sess.pages.remove(page_idx);
        if sess.active_page_idx >= sess.pages.len() {
            sess.active_page_idx = sess.pages.len() - 1;
        }
        Ok(())
    }

    pub async fn list_sessions(&self) -> Vec<SessionInfo> {
        let mut result = Vec::new();
        for (name, sess) in &self.sessions {
            let mut pages = Vec::new();
            for (i, page) in sess.pages.iter().enumerate() {
                let url = page.url().await.ok().flatten().unwrap_or_default();
                let title = page.get_title().await.ok().flatten().unwrap_or_default();
                pages.push(PageInfo {
                    index: i,
                    url,
                    title,
                    active: i == sess.active_page_idx,
                });
            }
            result.push(SessionInfo {
                name: name.clone(),
                pages,
            });
        }
        result.sort_by(|a, b| a.name.cmp(&b.name));
        result
    }

    pub fn set_ref_map(&mut self, session: &str, ref_map: HashMap<String, i64>) {
        if let Some(sess) = self.sessions.get_mut(session) {
            sess.ref_map = ref_map;
        }
    }

    pub fn ref_map(&self, session: &str) -> HashMap<String, i64> {
        self.sessions
            .get(session)
            .map(|s| s.ref_map.clone())
            .unwrap_or_default()
    }

    /// Get console messages for a session.
    pub async fn console_messages(&self, session: &str, level: Option<&str>, limit: usize) -> Result<Vec<ConsoleMsg>, String> {
        let sess = self.session(session)?;
        let msgs = sess.console_log.read().await;
        let filtered: Vec<ConsoleMsg> = msgs
            .iter()
            .filter(|m| level.map_or(true, |l| l == "all" || m.level == l))
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Ok(filtered)
    }

    /// Get network requests for a session.
    pub async fn network_requests(&self, session: &str, limit: usize) -> Result<Vec<NetRequest>, String> {
        let sess = self.session(session)?;
        let reqs = sess.network_log.read().await;
        let result: Vec<NetRequest> = reqs
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Ok(result)
    }

    pub async fn shutdown(&mut self) {
        self.sessions.clear();
        if let Some(browser) = self.browser.take() {
            let _ = browser.close().await;
        }
        if let Some(handle) = self.handler_handle.take() {
            handle.abort();
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionInfo {
    pub name: String,
    pub pages: Vec<PageInfo>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PageInfo {
    pub index: usize,
    pub url: String,
    pub title: String,
    pub active: bool,
}

/// Discover the WebSocket URL from an HTTP debug endpoint (e.g. http://localhost:9222).
async fn discover_ws_from_http(http_url: &str) -> Result<String, String> {
    let url = http_url.trim_end_matches('/');
    let host_port = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("Expected http:// URL, got {http_url}"))?;

    let stream = tokio::net::TcpStream::connect(host_port)
        .await
        .map_err(|e| format!("Cannot connect to {host_port}: {e}"))?;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (reader, mut writer) = stream.into_split();
    let req = format!(
        "GET /json/version HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n"
    );
    writer.write_all(req.as_bytes()).await.map_err(|e| format!("Write: {e}"))?;

    // Read headers to find Content-Length, then read body
    let mut buf_reader = BufReader::new(reader);
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        buf_reader.read_line(&mut line).await.map_err(|e| format!("Read header: {e}"))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break; // End of headers
        }
        if let Some(val) = trimmed.strip_prefix("Content-Length:") {
            content_length = val.trim().parse().unwrap_or(0);
        }
        if let Some(val) = trimmed.strip_prefix("content-length:") {
            content_length = val.trim().parse().unwrap_or(0);
        }
    }

    // Read body
    use tokio::io::AsyncReadExt;
    let mut body = vec![0u8; content_length.max(4096)];
    let n = buf_reader.read(&mut body).await.map_err(|e| format!("Read body: {e}"))?;
    let body_str = String::from_utf8_lossy(&body[..n]);

    let json: serde_json::Value =
        serde_json::from_str(&body_str).map_err(|e| format!("Parse /json/version: {e}"))?;

    json.get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "No webSocketDebuggerUrl in /json/version".to_string())
}

/// Discover a running Chrome instance by reading its DevToolsActivePort file.
/// This is the same mechanism Chrome DevTools MCP uses for --autoConnect.
///
/// Chrome M144+ creates this file when remote debugging is enabled via
/// chrome://inspect/#remote-debugging. It contains:
///   Line 1: port number
///   Line 2: WebSocket path (/devtools/browser/...)
fn discover_chrome_ws(channel: &str, explicit_user_data_dir: Option<&str>) -> Result<String, String> {
    let user_data_dir = if let Some(dir) = explicit_user_data_dir {
        std::path::PathBuf::from(dir)
    } else {
        chrome_default_user_data_dir(channel)?
    };

    let port_file = user_data_dir.join("DevToolsActivePort");
    let content = std::fs::read_to_string(&port_file).map_err(|e| {
        format!(
            "Cannot read {}: {e}. Ensure Chrome ({channel}) is running and \
             remote debugging is enabled at chrome://inspect/#remote-debugging",
            port_file.display()
        )
    })?;

    let lines: Vec<&str> = content.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    if lines.len() < 2 {
        return Err(format!("Invalid DevToolsActivePort content: {content:?}"));
    }

    let port: u16 = lines[0]
        .parse()
        .map_err(|_| format!("Invalid port '{}' in DevToolsActivePort", lines[0]))?;
    let path = lines[1];

    Ok(format!("ws://127.0.0.1:{port}{path}"))
}

/// Find Chrome's default user data directory for a given channel.
fn chrome_default_user_data_dir(channel: &str) -> Result<std::path::PathBuf, String> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| "Cannot determine home directory".to_string())?;

    let os = std::env::consts::OS;
    let suffix = match channel {
        "stable" | "chrome" => "",
        "beta" => " Beta",
        "dev" => " Dev",
        "canary" => " Canary",
        other => return Err(format!("Unknown Chrome channel: {other}")),
    };

    let path = match os {
        "linux" => {
            let dir_name = if suffix.is_empty() {
                "google-chrome".to_string()
            } else {
                format!("google-chrome{}", suffix.to_lowercase().replace(' ', "-"))
            };
            std::path::PathBuf::from(&home).join(".config").join(dir_name)
        }
        "macos" => {
            std::path::PathBuf::from(&home)
                .join("Library/Application Support")
                .join(format!("Google/Chrome{suffix}"))
        }
        "windows" => {
            let local_app_data = std::env::var("LOCALAPPDATA")
                .unwrap_or_else(|_| format!("{home}/AppData/Local"));
            std::path::PathBuf::from(local_app_data)
                .join(format!("Google/Chrome{suffix}/User Data"))
        }
        _ => return Err(format!("Unsupported OS: {os}")),
    };

    if !path.exists() {
        // Also check for chromium
        let chromium_path = match os {
            "linux" => std::path::PathBuf::from(&home).join(".config/chromium"),
            "macos" => std::path::PathBuf::from(&home).join("Library/Application Support/Chromium"),
            _ => return Err(format!("Chrome user data dir not found: {}", path.display())),
        };
        if chromium_path.exists() {
            return Ok(chromium_path);
        }
        return Err(format!(
            "Chrome user data dir not found at {} or {}",
            path.display(),
            chromium_path.display()
        ));
    }

    Ok(path)
}

fn detect_chromium() -> String {
    for path in &[
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/google-chrome-stable",
        "/usr/bin/google-chrome",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    ] {
        if std::path::Path::new(path).exists() {
            return path.to_string();
        }
    }
    "chromium".to_string()
}
