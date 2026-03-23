//! Browser state management with session-scoped isolation.
//!
//! Design principles:
//! - No global "active page" — every tool call specifies its session
//! - Sessions map to isolated browser contexts (isolated cookies, storage, websockets)
//! - No races possible: there is no shared mutable selection state
//! - Single-threaded tokio runtime: Mutex is never actually contended

use crate::backend::{AnyBrowser, AnyPage, BackendKind};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Default viewport dimensions -- consistent across all backends.
pub const DEFAULT_VIEWPORT_WIDTH: i64 = 1280;
pub const DEFAULT_VIEWPORT_HEIGHT: i64 = 720;

/// A collected console message.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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

/// A dismissed dialog event (alert, confirm, prompt).
#[derive(Debug, Clone, serde::Serialize)]
pub struct DialogEvent {
    pub dialog_type: String,
    pub message: String,
    pub action: String,
}

/// A single isolated session backed by a browser context.
pub struct Session {
    pub pages: Vec<AnyPage>,
    pub active_page_idx: usize,
    pub ref_map: HashMap<String, i64>,
    /// Console messages collected from events.
    pub console_log: Arc<RwLock<Vec<ConsoleMsg>>>,
    /// Network requests collected from events.
    pub network_log: Arc<RwLock<Vec<NetRequest>>>,
    /// Dialog events (auto-dismissed alerts, confirms, prompts).
    pub dialog_log: Arc<RwLock<Vec<DialogEvent>>>,
}

impl Session {
    fn new() -> Self {
        Self {
            pages: Vec::new(),
            active_page_idx: 0,
            ref_map: HashMap::new(),
            console_log: Arc::new(RwLock::new(Vec::new())),
            network_log: Arc::new(RwLock::new(Vec::new())),
            dialog_log: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn active_page(&self) -> Option<&AnyPage> {
        self.pages.get(self.active_page_idx)
    }
}

/// All browser state.
pub struct BrowserState {
    browser: Option<AnyBrowser>,
    sessions: HashMap<String, Session>,
    chromium_path: String,
    /// Connection mode
    connect_mode: ConnectMode,
    /// Backend kind
    backend_kind: BackendKind,
}

#[derive(Clone)]
pub enum ConnectMode {
    /// Launch a new browser (default)
    Launch,
    /// Connect to browser at explicit ws:// or http:// URL
    ConnectUrl(String),
    /// Auto-connect to running Chrome by reading DevToolsActivePort file
    AutoConnect {
        channel: String,
        user_data_dir: Option<String>,
    },
}

impl BrowserState {
    pub fn new(connect_mode: ConnectMode, backend_kind: BackendKind) -> Self {
        let chromium_path =
            std::env::var("CHROMIUM_PATH").unwrap_or_else(|_| detect_chromium());
        Self {
            browser: None,
            sessions: HashMap::new(),
            chromium_path,
            connect_mode,
            backend_kind,
        }
    }

    /// Ensure browser is launched or connected.
    pub async fn ensure_browser(&mut self) -> Result<(), String> {
        if self.browser.is_some() {
            return Ok(());
        }

        let browser = match self.backend_kind {
            BackendKind::CdpWs => {
                use crate::backend::cdp_ws::CdpWsBrowser;
                match &self.connect_mode {
                    ConnectMode::ConnectUrl(url) => {
                        let ws_url = if url.starts_with("ws://") || url.starts_with("wss://") {
                            url.clone()
                        } else {
                            discover_ws_from_http(url).await?
                        };
                        AnyBrowser::CdpWs(CdpWsBrowser::connect(&ws_url).await?)
                    }
                    ConnectMode::AutoConnect {
                        channel,
                        user_data_dir,
                    } => {
                        let ws_url =
                            discover_chrome_ws(channel, user_data_dir.as_deref())?;
                        eprintln!("Auto-connecting to Chrome at {ws_url}");
                        AnyBrowser::CdpWs(CdpWsBrowser::connect(&ws_url).await.map_err(|e| {
                            format!(
                                "Auto-connect failed: {e}. Ensure Chrome is running and remote \
                                 debugging is enabled at chrome://inspect/#remote-debugging"
                            )
                        })?)
                    }
                    ConnectMode::Launch => {
                        AnyBrowser::CdpWs(CdpWsBrowser::launch(&self.chromium_path).await?)
                    }
                }
            }
            BackendKind::CdpPipe => {
                use crate::backend::cdp_pipe::CdpPipeBrowser;
                AnyBrowser::CdpPipe(CdpPipeBrowser::launch(&self.chromium_path).await?)
            }
            BackendKind::CdpRaw => {
                use crate::backend::cdp_raw::CdpRawBrowser;
                AnyBrowser::CdpRaw(CdpRawBrowser::launch(&self.chromium_path).await?)
            }
            #[cfg(target_os = "macos")]
            BackendKind::WebKit => {
                use crate::backend::webkit::WebKitBrowser;
                AnyBrowser::WebKit(WebKitBrowser::launch().await?)
            }
        };

        self.browser = Some(browser);

        // Discover existing pages or create one
        let existing_pages = self.browser().pages().await.unwrap_or_default();
        let pages = if existing_pages.is_empty() {
            let page = self.browser().new_page("about:blank").await?;
            vec![page]
        } else {
            existing_pages
        };
        let sess = self.session_mut("default");
        for page in pages {
            // Set standard viewport (1280x720) on every backend for consistent behavior
            let _ = page.emulate_viewport(&crate::options::ViewportConfig::default()).await;
            page.attach_listeners(sess.console_log.clone(), sess.network_log.clone(), sess.dialog_log.clone());
            sess.pages.push(page);
        }

        Ok(())
    }

    fn browser(&self) -> &AnyBrowser {
        self.browser.as_ref().expect("Browser not launched")
    }

    fn session_mut(&mut self, name: &str) -> &mut Session {
        self.sessions
            .entry(name.to_string())
            .or_insert_with(Session::new)
    }

    pub fn session(&self, name: &str) -> Result<&Session, String> {
        self.sessions.get(name).ok_or_else(|| {
            format!(
                "Session '{name}' not found. Use new_page with session='{name}' to create it."
            )
        })
    }

    pub fn session_mut_checked(&mut self, name: &str) -> Result<&mut Session, String> {
        self.sessions
            .get_mut(name)
            .ok_or_else(|| format!("Session '{name}' not found."))
    }

    /// Open a new page in a session.
    pub async fn open_page(&mut self, session: &str, url: &str) -> Result<usize, String> {
        self.ensure_browser().await?;

        let page = if session == "default" {
            self.browser().new_page(url).await?
        } else {
            self.browser().new_page_isolated(url).await?
        };

        // Don't call wait_for_navigation here — the backend's new_page/goto
        // already waits for the page to load (Page.loadEventFired for CDP,
        // navigation delegate for WebKit). Double-waiting would hang because
        // the loadEventFired event is already consumed.

        // Set standard viewport on new pages
        let _ = page.emulate_viewport(&crate::options::ViewportConfig::default()).await;

        let sess = self.session_mut(session);
        page.attach_listeners(sess.console_log.clone(), sess.network_log.clone(), sess.dialog_log.clone());
        let idx = sess.pages.len();
        sess.pages.push(page);
        sess.active_page_idx = idx;

        Ok(idx)
    }

    pub fn active_page(&self, session: &str) -> Result<&AnyPage, String> {
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
                let title = page.title().await.ok().flatten().unwrap_or_default();
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
    pub async fn console_messages(
        &self,
        session: &str,
        level: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ConsoleMsg>, String> {
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
    pub async fn network_requests(
        &self,
        session: &str,
        limit: usize,
    ) -> Result<Vec<NetRequest>, String> {
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

    /// Refresh the page list for a session by querying the browser for current targets.
    /// Adds any new pages that weren't previously tracked.
    pub async fn refresh_pages(&mut self, session: &str) -> Result<usize, String> {
        let browser = self.browser.as_ref().ok_or("No browser")?;
        let current_pages = browser.pages().await?;
        let sess = self.sessions.get_mut(session).ok_or_else(|| format!("Session '{session}' not found"))?;

        let existing_count = sess.pages.len();
        // Find pages that exist in browser but not in our session
        // Simple heuristic: if browser has more pages than we track, add the extras
        if current_pages.len() > existing_count {
            for page in current_pages.into_iter().skip(existing_count) {
                page.attach_listeners(sess.console_log.clone(), sess.network_log.clone(), sess.dialog_log.clone());
                sess.pages.push(page);
            }
        }
        Ok(sess.pages.len())
    }

    /// Get dialog messages for a session.
    pub async fn dialog_messages(
        &self,
        session: &str,
        limit: usize,
    ) -> Result<Vec<DialogEvent>, String> {
        let sess = self.session(session)?;
        let msgs = sess.dialog_log.read().await;
        let start = msgs.len().saturating_sub(limit);
        Ok(msgs[start..].to_vec())
    }

    pub async fn shutdown(&mut self) {
        self.sessions.clear();
        if let Some(mut browser) = self.browser.take() {
            let _ = browser.close().await;
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

/// Discover the WebSocket URL from an HTTP debug endpoint.
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
    writer
        .write_all(req.as_bytes())
        .await
        .map_err(|e| format!("Write: {e}"))?;

    let mut buf_reader = BufReader::new(reader);
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        buf_reader
            .read_line(&mut line)
            .await
            .map_err(|e| format!("Read header: {e}"))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some(val) = trimmed.strip_prefix("Content-Length:") {
            content_length = val.trim().parse().unwrap_or(0);
        }
        if let Some(val) = trimmed.strip_prefix("content-length:") {
            content_length = val.trim().parse().unwrap_or(0);
        }
    }

    use tokio::io::AsyncReadExt;
    let mut body = vec![0u8; content_length.max(4096)];
    let n = buf_reader
        .read(&mut body)
        .await
        .map_err(|e| format!("Read body: {e}"))?;
    let body_str = String::from_utf8_lossy(&body[..n]);

    let json: serde_json::Value =
        serde_json::from_str(&body_str).map_err(|e| format!("Parse /json/version: {e}"))?;

    json.get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "No webSocketDebuggerUrl in /json/version".to_string())
}

/// Discover a running Chrome instance by reading its DevToolsActivePort file.
fn discover_chrome_ws(
    channel: &str,
    explicit_user_data_dir: Option<&str>,
) -> Result<String, String> {
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

    let lines: Vec<&str> = content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() < 2 {
        return Err(format!(
            "Invalid DevToolsActivePort content: {content:?}"
        ));
    }

    let port: u16 = lines[0]
        .parse()
        .map_err(|_| format!("Invalid port '{}' in DevToolsActivePort", lines[0]))?;
    let path = lines[1];

    Ok(format!("ws://127.0.0.1:{port}{path}"))
}

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
                format!(
                    "google-chrome{}",
                    suffix.to_lowercase().replace(' ', "-")
                )
            };
            std::path::PathBuf::from(&home).join(".config").join(dir_name)
        }
        "macos" => std::path::PathBuf::from(&home)
            .join("Library/Application Support")
            .join(format!("Google/Chrome{suffix}")),
        "windows" => {
            let local_app_data = std::env::var("LOCALAPPDATA")
                .unwrap_or_else(|_| format!("{home}/AppData/Local"));
            std::path::PathBuf::from(local_app_data)
                .join(format!("Google/Chrome{suffix}/User Data"))
        }
        _ => return Err(format!("Unsupported OS: {os}")),
    };

    if !path.exists() {
        let chromium_path = match os {
            "linux" => std::path::PathBuf::from(&home).join(".config/chromium"),
            "macos" => {
                std::path::PathBuf::from(&home).join("Library/Application Support/Chromium")
            }
            _ => {
                return Err(format!(
                    "Chrome user data dir not found: {}",
                    path.display()
                ))
            }
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

/// Common Chrome/Chromium launch flags used by both cdp-ws and cdp-pipe backends.
/// Matches the flags Bun uses in ChromeProcess.zig for maximum compatibility.
pub const CHROME_FLAGS: &[&str] = &[
    "--headless",
    "--no-sandbox",
    "--disable-gpu",
    "--disable-dev-shm-usage",
    "--disable-extensions",
    "--disable-background-networking",
    "--disable-background-timer-throttling",
    "--disable-backgrounding-occluded-windows",
    "--disable-renderer-backgrounding",
    "--disable-ipc-flooding-protection",
    "--disable-sync",
    "--disable-translate",
    "--disable-default-apps",
    "--disable-component-update",
    "--no-first-run",
    "--no-default-browser-check",
];

/// Detect Chrome/Chromium binary on the system.
/// Follows the same priority as Bun's findChrome():
///   1. CHROMIUM_PATH env var
///   2. $PATH search (google-chrome-stable, google-chrome, chromium-browser, chromium, microsoft-edge, chrome)
///   3. Hardcoded app bundle paths (macOS /Applications, ~/Applications)
///   4. Hardcoded Linux paths (/usr/bin, /snap/bin)
///   5. Playwright cache (chromium_headless_shell)
pub fn detect_chromium() -> String {
    // 1. Env var (already handled by caller, but check here too for completeness)
    if let Ok(p) = std::env::var("CHROMIUM_PATH") {
        if std::path::Path::new(&p).exists() {
            return p;
        }
    }

    // 2. $PATH search — same names and order as Bun
    if let Ok(path_var) = std::env::var("PATH") {
        let names = [
            "google-chrome-stable",
            "google-chrome",
            "chromium-browser",
            "chromium",
            "microsoft-edge",
            "chrome",
        ];
        for name in &names {
            for dir in path_var.split(':') {
                let candidate = std::path::PathBuf::from(dir).join(name);
                if candidate.exists() {
                    return candidate.to_string_lossy().to_string();
                }
            }
        }
    }

    // 3. Hardcoded paths
    #[cfg(target_os = "macos")]
    {
        let bundles = [
            "Google Chrome.app/Contents/MacOS/Google Chrome",
            "Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
            "Chromium.app/Contents/MacOS/Chromium",
            "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ];
        for bundle in &bundles {
            // /Applications
            let sys = std::path::PathBuf::from("/Applications").join(bundle);
            if sys.exists() {
                return sys.to_string_lossy().to_string();
            }
            // ~/Applications
            if let Ok(home) = std::env::var("HOME") {
                let user = std::path::PathBuf::from(&home).join("Applications").join(bundle);
                if user.exists() {
                    return user.to_string_lossy().to_string();
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        let paths = [
            "/usr/bin/google-chrome-stable",
            "/usr/bin/google-chrome",
            "/usr/bin/chromium-browser",
            "/usr/bin/chromium",
            "/snap/bin/chromium",
            "/usr/bin/microsoft-edge",
        ];
        for path in &paths {
            if std::path::Path::new(path).exists() {
                return path.to_string();
            }
        }
    }

    // 4. Playwright cache — look for chromium_headless_shell-<rev>
    if let Some(p) = find_playwright_chrome() {
        return p;
    }

    "chromium".to_string()
}

/// Search Playwright's cache dir for a chromium headless shell binary.
fn find_playwright_chrome() -> Option<String> {
    let home = std::env::var("HOME").ok()?;

    #[cfg(target_os = "macos")]
    let cache_dir = std::path::PathBuf::from(&home).join("Library/Caches/ms-playwright");
    #[cfg(target_os = "linux")]
    let cache_dir = std::path::PathBuf::from(&home).join(".cache/ms-playwright");
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return None;

    if !cache_dir.exists() {
        return None;
    }

    // Find the newest chromium_headless_shell-<rev> directory
    let mut best_rev: u32 = 0;
    let mut best_name = String::new();
    let prefix = "chromium_headless_shell-";

    if let Ok(entries) = std::fs::read_dir(&cache_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(rev_str) = name.strip_prefix(prefix) {
                if let Ok(rev) = rev_str.parse::<u32>() {
                    if rev > best_rev {
                        best_rev = rev;
                        best_name = name;
                    }
                }
            }
        }
    }

    if best_rev == 0 {
        return None;
    }

    // Build binary path — two possible layouts
    #[cfg(target_os = "macos")]
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
    #[cfg(target_os = "linux")]
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };

    #[cfg(target_os = "macos")]
    let plat = "mac";
    #[cfg(target_os = "linux")]
    let plat = "linux";

    let cft_binary = cache_dir
        .join(&best_name)
        .join(format!("chrome-headless-shell-{plat}-{arch}"))
        .join("chrome-headless-shell");

    if cft_binary.exists() {
        return Some(cft_binary.to_string_lossy().to_string());
    }

    // Non-cft layout (Linux arm64)
    #[cfg(target_os = "linux")]
    {
        let alt_binary = cache_dir
            .join(&best_name)
            .join("chrome-linux")
            .join("headless_shell");
        if alt_binary.exists() {
            return Some(alt_binary.to_string_lossy().to_string());
        }
    }

    None
}
