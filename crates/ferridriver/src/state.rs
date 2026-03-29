//! Browser state management with instance→context→page hierarchy.
//!
//! Design principles:
//! - Instance = Chrome process (owns chrome flags)
//! - Context = isolated browser context within an instance (isolated cookies, storage)
//! - Page = tab within a context
//! - Composite session key: "<instance>:<context>" (backwards compat: bare name = default instance)
//! - No global "active page" -- every tool call specifies its session key
//! - No races possible: there is no shared mutable selection state

use crate::backend::{AnyBrowser, AnyPage, BackendKind};
use crate::context::BrowserContext;
use rustc_hash::FxHashMap as HashMap;

/// Default viewport dimensions -- consistent across all backends.
pub const DEFAULT_VIEWPORT_WIDTH: i64 = 1280;
pub const DEFAULT_VIEWPORT_HEIGHT: i64 = 720;

// Re-export log types from context (they live there now).
pub use crate::context::{ConsoleMsg, DialogEvent, NetRequest};

// ── SessionKey ──────────────────────────────────────────────────────────────

/// Parsed composite session key: `"<instance>:<context>"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionKey {
    pub instance: String,
    pub context: String,
}

impl SessionKey {
    /// Parse a composite key string.
    ///
    /// - `"default"` → instance="default", context="default"
    /// - `"myctx"` → instance="default", context="myctx"
    /// - `"staging:admin"` → instance="staging", context="admin"
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        if let Some((inst, ctx)) = raw.split_once(':') {
            SessionKey {
                instance: inst.to_string(),
                context: ctx.to_string(),
            }
        } else if raw == "default" {
            SessionKey {
                instance: "default".to_string(),
                context: "default".to_string(),
            }
        } else {
            // Backwards compat: bare name → default instance, name as context
            SessionKey {
                instance: "default".to_string(),
                context: raw.to_string(),
            }
        }
    }

    /// Reconstruct the composite key string.
    #[must_use]
    pub fn to_composite(&self) -> String {
        format!("{}:{}", self.instance, self.context)
    }
}

// ── BrowserInstance ─────────────────────────────────────────────────────────

/// A single Chrome process and its isolated contexts.
struct BrowserInstance {
    browser: AnyBrowser,
    contexts: HashMap<String, BrowserContext>,
}

impl BrowserInstance {
    fn context(&self, name: &str) -> Result<&BrowserContext, String> {
        self.contexts.get(name).ok_or_else(|| {
            format!("Context '{name}' not found in this instance.")
        })
    }

    fn context_mut(&mut self, name: &str) -> &mut BrowserContext {
        self.contexts
            .entry(name.to_string())
            .or_insert_with(|| BrowserContext::new(name.to_string()))
    }

    fn context_mut_checked(&mut self, name: &str) -> Result<&mut BrowserContext, String> {
        self.contexts
            .get_mut(name)
            .ok_or_else(|| format!("Context '{name}' not found."))
    }

    fn remove_context(&mut self, name: &str) {
        self.contexts.remove(name);
    }
}

// ── BrowserState ────────────────────────────────────────────────────────────

/// Callback type for per-instance chrome args.
pub type InstanceArgsFn = Box<dyn Fn(&str) -> Vec<String> + Send + Sync>;

/// All browser state -- manages multiple Chrome instances, each with contexts and pages.
pub struct BrowserState {
    instances: HashMap<String, BrowserInstance>,
    chromium_path: String,
    connect_mode: ConnectMode,
    backend_kind: BackendKind,
    /// Base Chrome flags applied to ALL instances.
    pub extra_args: Vec<String>,
    /// Per-instance additional chrome args. Called with instance name when launching.
    instance_args_fn: Option<InstanceArgsFn>,
    /// Whether to run headless.
    pub headless: bool,
    /// Custom user data directory.
    pub user_data_dir: Option<String>,
    /// Default viewport for new pages.
    pub default_viewport: Option<crate::options::ViewportConfig>,
}

#[derive(Clone)]
pub enum ConnectMode {
    /// Launch a new browser (default)
    Launch,
    /// Connect to browser at explicit ws:// or http:// URL
    ConnectUrl(String),
    /// Auto-connect to running Chrome by reading `DevToolsActivePort` file
    AutoConnect {
        channel: String,
        user_data_dir: Option<String>,
    },
}

impl BrowserState {
    #[must_use]
    pub fn new(connect_mode: ConnectMode, backend_kind: BackendKind) -> Self {
        let chromium_path =
            std::env::var("CHROMIUM_PATH").unwrap_or_else(|_| detect_chromium());
        Self {
            instances: HashMap::default(),
            chromium_path,
            connect_mode,
            backend_kind,
            extra_args: Vec::new(),
            instance_args_fn: None,
            headless: true,
            user_data_dir: None,
            default_viewport: Some(crate::options::ViewportConfig::default()),
        }
    }

    /// Create state from `LaunchOptions`.
    #[must_use]
    pub fn with_options(connect_mode: ConnectMode, opts: crate::options::LaunchOptions) -> Self {
        let chromium_path = opts.executable_path.unwrap_or_else(||
            std::env::var("CHROMIUM_PATH").unwrap_or_else(|_| detect_chromium())
        );
        Self {
            instances: HashMap::default(),
            chromium_path,
            connect_mode,
            backend_kind: opts.backend,
            extra_args: opts.args,
            instance_args_fn: None,
            headless: opts.headless,
            user_data_dir: opts.user_data_dir,
            default_viewport: opts.viewport,
        }
    }

    /// Set a callback for per-instance additional chrome args.
    /// Called with the instance name when launching a new Chrome process.
    pub fn set_instance_args_fn(&mut self, f: InstanceArgsFn) {
        self.instance_args_fn = Some(f);
    }

    // ── Instance management ─────────────────────────────────────────────────

    /// Ensure a Chrome instance is launched. If it already exists, no-op.
    ///
    /// # Errors
    ///
    /// Returns an error if the browser process fails to start or connection fails.
    pub async fn ensure_instance(&mut self, instance_name: &str) -> Result<(), String> {
        if self.instances.contains_key(instance_name) {
            return Ok(());
        }

        // Build flags: base + per-instance
        let mut all_extra = self.extra_args.clone();
        if let Some(ref f) = self.instance_args_fn {
            all_extra.extend(f(instance_name));
        }

        let browser = match self.backend_kind {
            BackendKind::CdpPipe => {
                use crate::backend::cdp_pipe::CdpPipeBrowser;
                let flags = chrome_flags(self.headless, &all_extra);
                AnyBrowser::CdpPipe(CdpPipeBrowser::launch_with_flags(&self.chromium_path, &flags).await?)
            }
            BackendKind::CdpRaw => {
                use crate::backend::cdp_raw::CdpRawBrowser;
                match &self.connect_mode {
                    ConnectMode::ConnectUrl(url) => {
                        let ws_url = if url.starts_with("ws://") || url.starts_with("wss://") {
                            url.clone()
                        } else {
                            discover_ws_from_http(url).await?
                        };
                        AnyBrowser::CdpRaw(CdpRawBrowser::connect(&ws_url).await?)
                    }
                    ConnectMode::AutoConnect { channel, user_data_dir } => {
                        let ws_url = discover_chrome_ws(channel, user_data_dir.as_deref())?;
                        AnyBrowser::CdpRaw(CdpRawBrowser::connect(&ws_url).await?)
                    }
                    ConnectMode::Launch => {
                        let flags = chrome_flags(self.headless, &all_extra);
                        AnyBrowser::CdpRaw(CdpRawBrowser::launch_with_flags(&self.chromium_path, &flags).await?)
                    }
                }
            }
            #[cfg(target_os = "macos")]
            BackendKind::WebKit => {
                use crate::backend::webkit::WebKitBrowser;
                AnyBrowser::WebKit(WebKitBrowser::launch().await?)
            }
        };

        let mut inst = BrowserInstance {
            browser,
            contexts: HashMap::default(),
        };

        // Adopt existing pages into the "default" context of this instance.
        let existing_pages = inst.browser.pages().await.unwrap_or_default();
        let vp = self.default_viewport.clone().unwrap_or_default();
        let ctx = inst.context_mut("default");
        if !existing_pages.is_empty() {
            for page in existing_pages {
                let _ = page.emulate_viewport(&vp).await;
                page.attach_listeners(ctx.console_log.clone(), ctx.network_log.clone(), ctx.dialog_log.clone());
                ctx.pages.push(page);
            }
        }
        if ctx.pages.is_empty() {
            let page = inst.browser.new_page("about:blank").await?;
            let _ = page.emulate_viewport(&vp).await;
            let ctx = inst.context_mut("default");
            page.attach_listeners(ctx.console_log.clone(), ctx.network_log.clone(), ctx.dialog_log.clone());
            ctx.pages.push(page);
        }

        self.instances.insert(instance_name.to_string(), inst);
        Ok(())
    }

    /// Backwards-compat: ensure the "default" instance.
    ///
    /// # Errors
    ///
    /// Returns an error if the browser process fails to start.
    pub async fn ensure_browser(&mut self) -> Result<(), String> {
        Box::pin(self.ensure_instance("default")).await
    }

    /// Connect to a running browser at the given WebSocket or HTTP URL.
    /// Creates a new instance with the given name using `CdpRaw` backend.
    ///
    /// # Errors
    ///
    /// Returns an error if the WebSocket connection or page discovery fails.
    pub async fn connect_to_url(&mut self, instance_name: &str, url: &str) -> Result<usize, String> {
        use crate::backend::cdp_raw::CdpRawBrowser;

        // Drop existing instance if any
        self.instances.remove(instance_name);

        let ws_url = if url.starts_with("ws://") || url.starts_with("wss://") {
            url.to_string()
        } else {
            discover_ws_from_http(url).await?
        };

        let browser = AnyBrowser::CdpRaw(CdpRawBrowser::connect(&ws_url).await?);
        let mut inst = BrowserInstance {
            browser,
            contexts: HashMap::default(),
        };

        let vp = self.default_viewport.clone().unwrap_or_default();
        let existing_pages = inst.browser.pages().await.unwrap_or_default();
        let ctx = inst.context_mut("default");
        let page_count = existing_pages.len();
        for page in existing_pages {
            let _ = page.emulate_viewport(&vp).await;
            page.attach_listeners(ctx.console_log.clone(), ctx.network_log.clone(), ctx.dialog_log.clone());
            ctx.pages.push(page);
        }

        self.instances.insert(instance_name.to_string(), inst);
        Ok(page_count)
    }

    /// Auto-discover and connect to a running Chrome instance.
    /// Reads Chrome's `DevToolsActivePort` file to find the WebSocket URL.
    ///
    /// # Errors
    ///
    /// Returns an error if Chrome discovery or connection fails.
    pub async fn connect_auto(&mut self, instance_name: &str, channel: &str, user_data_dir: Option<&str>) -> Result<usize, String> {
        let ws_url = discover_chrome_ws(channel, user_data_dir)?;
        Box::pin(self.connect_to_url(instance_name, &ws_url)).await
    }

    // ── Routing helpers ─────────────────────────────────────────────────────

    fn instance(&self, name: &str) -> Result<&BrowserInstance, String> {
        self.instances.get(name).ok_or_else(|| {
            format!("Browser instance '{name}' not found. It will be created on first use.")
        })
    }

    fn instance_mut(&mut self, name: &str) -> Result<&mut BrowserInstance, String> {
        self.instances.get_mut(name).ok_or_else(|| {
            format!("Browser instance '{name}' not found.")
        })
    }

    // ── Public methods (all parse composite keys) ───────────────────────────

    /// Open a new page in a context. `context` is a composite key like `"staging:admin"`.
    ///
    /// # Errors
    ///
    /// Returns an error if the instance or page creation fails.
    pub async fn open_page(&mut self, context: &str, url: &str) -> Result<usize, String> {
        let key = SessionKey::parse(context);
        Box::pin(self.ensure_instance(&key.instance)).await?;

        let vp = self.default_viewport.clone().unwrap_or_default();
        let inst = self.instance_mut(&key.instance)?;

        let page = if key.context == "default" {
            inst.browser.new_page(url).await?
        } else {
            inst.browser.new_page_isolated(url).await?
        };

        let _ = page.emulate_viewport(&vp).await;

        let ctx = inst.context_mut(&key.context);
        page.attach_listeners(ctx.console_log.clone(), ctx.network_log.clone(), ctx.dialog_log.clone());
        let idx = ctx.pages.len();
        ctx.pages.push(page);
        ctx.active_page_idx = idx;

        Ok(idx)
    }

    /// # Errors
    ///
    /// Returns an error if the instance, context, or page does not exist.
    pub fn active_page(&self, context: &str) -> Result<&AnyPage, String> {
        let key = SessionKey::parse(context);
        let inst = self.instance(&key.instance)?;
        let ctx = inst.context(&key.context)?;
        ctx.active_page()
            .ok_or_else(|| format!("No pages in context '{context}'"))
    }

    /// # Errors
    ///
    /// Returns an error if the instance or context does not exist.
    pub fn context(&self, context: &str) -> Result<&BrowserContext, String> {
        let key = SessionKey::parse(context);
        let inst = self.instance(&key.instance)?;
        inst.context(&key.context)
    }

    /// # Errors
    ///
    /// Returns an error if the instance or context does not exist.
    pub fn context_mut_checked(&mut self, context: &str) -> Result<&mut BrowserContext, String> {
        let key = SessionKey::parse(context);
        let inst = self.instance_mut(&key.instance)?;
        inst.context_mut_checked(&key.context)
    }

    pub fn remove_context(&mut self, context: &str) {
        let key = SessionKey::parse(context);
        if let Some(inst) = self.instances.get_mut(&key.instance) {
            inst.remove_context(&key.context);
        }
    }

    /// # Errors
    ///
    /// Returns an error if the context does not exist or the page index is out of range.
    pub fn select_page(&mut self, context: &str, page_idx: usize) -> Result<(), String> {
        let key = SessionKey::parse(context);
        let inst = self.instance_mut(&key.instance)?;
        let ctx = inst.context_mut_checked(&key.context)?;
        if page_idx >= ctx.pages.len() {
            return Err(format!(
                "Page index {page_idx} out of range (context '{context}' has {} pages)",
                ctx.pages.len()
            ));
        }
        ctx.active_page_idx = page_idx;
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error if this is the last page, context does not exist, or index is out of range.
    pub fn close_page(&mut self, context: &str, page_idx: usize) -> Result<(), String> {
        let key = SessionKey::parse(context);
        let inst = self.instance_mut(&key.instance)?;
        let ctx = inst.context_mut_checked(&key.context)?;
        if ctx.pages.len() <= 1 {
            return Err("Cannot close the last page in a context".into());
        }
        if page_idx >= ctx.pages.len() {
            return Err(format!("Page index {page_idx} out of range"));
        }
        ctx.pages.remove(page_idx);
        if ctx.active_page_idx >= ctx.pages.len() {
            ctx.active_page_idx = ctx.pages.len() - 1;
        }
        Ok(())
    }

    pub async fn list_contexts(&self) -> Vec<ContextInfo> {
        let mut result = Vec::new();
        for (inst_name, inst) in &self.instances {
            for (ctx_name, ctx) in &inst.contexts {
                let mut pages = Vec::new();
                for (i, page) in ctx.pages.iter().enumerate() {
                    let url = page.url().await.ok().flatten().unwrap_or_default();
                    let title = page.title().await.ok().flatten().unwrap_or_default();
                    pages.push(PageInfo {
                        index: i,
                        url,
                        title,
                        active: i == ctx.active_page_idx,
                    });
                }
                // Use composite name for non-default instances, bare name for default
                let name = if inst_name == "default" {
                    ctx_name.clone()
                } else {
                    format!("{inst_name}:{ctx_name}")
                };
                result.push(ContextInfo {
                    name,
                    instance: inst_name.clone(),
                    context: ctx_name.clone(),
                    pages,
                });
            }
        }
        result.sort_by(|a, b| a.name.cmp(&b.name));
        result
    }

    pub fn set_ref_map(&mut self, context: &str, ref_map: HashMap<String, i64>) {
        let key = SessionKey::parse(context);
        if let Some(inst) = self.instances.get_mut(&key.instance) {
            if let Some(ctx) = inst.contexts.get_mut(&key.context) {
                ctx.ref_map = ref_map;
            }
        }
    }

    #[must_use]
    pub fn ref_map(&self, context: &str) -> HashMap<String, i64> {
        let key = SessionKey::parse(context);
        self.instances
            .get(&key.instance)
            .and_then(|inst| inst.contexts.get(&key.context))
            .map(|c| c.ref_map.clone())
            .unwrap_or_default()
    }

    /// # Errors
    ///
    /// Returns an error if the instance or context does not exist.
    pub async fn console_messages(
        &self,
        context: &str,
        level: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ConsoleMsg>, String> {
        let key = SessionKey::parse(context);
        let inst = self.instance(&key.instance)?;
        let ctx = inst.context(&key.context)?;
        Ok(ctx.console_messages(level, limit).await)
    }

    /// # Errors
    ///
    /// Returns an error if the instance or context does not exist.
    pub async fn network_requests(
        &self,
        context: &str,
        limit: usize,
    ) -> Result<Vec<NetRequest>, String> {
        let key = SessionKey::parse(context);
        let inst = self.instance(&key.instance)?;
        let ctx = inst.context(&key.context)?;
        Ok(ctx.network_requests(limit).await)
    }

    /// # Errors
    ///
    /// Returns an error if the instance or context does not exist, or page discovery fails.
    pub async fn refresh_pages(&mut self, context: &str) -> Result<usize, String> {
        let key = SessionKey::parse(context);
        let inst = self.instance_mut(&key.instance)?;
        let current_pages = inst.browser.pages().await?;
        let ctx = inst.context_mut_checked(&key.context)?;

        let existing_count = ctx.pages.len();
        if current_pages.len() > existing_count {
            for page in current_pages.into_iter().skip(existing_count) {
                page.attach_listeners(ctx.console_log.clone(), ctx.network_log.clone(), ctx.dialog_log.clone());
                ctx.pages.push(page);
            }
        }
        Ok(ctx.pages.len())
    }

    /// # Errors
    ///
    /// Returns an error if the instance or context does not exist.
    pub async fn dialog_messages(
        &self,
        context: &str,
        limit: usize,
    ) -> Result<Vec<DialogEvent>, String> {
        let key = SessionKey::parse(context);
        let inst = self.instance(&key.instance)?;
        let ctx = inst.context(&key.context)?;
        Ok(ctx.dialog_messages(limit).await)
    }

    pub async fn shutdown(&mut self) {
        for (_, mut inst) in self.instances.drain() {
            inst.contexts.clear();
            let _ = inst.browser.close().await;
        }
    }

    #[must_use]
    pub fn is_connected(&self) -> bool {
        !self.instances.is_empty()
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextInfo {
    pub name: String,
    pub instance: String,
    pub context: String,
    pub pages: Vec<PageInfo>,
}

// Backward-compat alias for code that still references SessionInfo.
pub type SessionInfo = ContextInfo;

#[derive(Debug, Clone, serde::Serialize)]
pub struct PageInfo {
    pub index: usize,
    pub url: String,
    pub title: String,
    pub active: bool,
}

/// Discover the WebSocket URL from an HTTP debug endpoint.
async fn discover_ws_from_http(http_url: &str) -> Result<String, String> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    let url = http_url.trim_end_matches('/');
    let host_port = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("Expected http:// URL, got {http_url}"))?;

    let stream = tokio::net::TcpStream::connect(host_port)
        .await
        .map_err(|e| format!("Cannot connect to {host_port}: {e}"))?;
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
        .map(std::string::ToString::to_string)
        .ok_or_else(|| "No webSocketDebuggerUrl in /json/version".to_string())
}

/// Discover a running Chrome instance by reading its `DevToolsActivePort` file.
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
        .map(str::trim)
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

/// Common Chrome/Chromium launch flags used by cdp-pipe and cdp-raw backends.
#[must_use]
pub fn chrome_flags(headless: bool, extra_args: &[String]) -> Vec<String> {
    let mut flags: Vec<String> = Vec::with_capacity(20 + extra_args.len());
    if headless {
        flags.push("--headless".into());
    }
    for f in BASE_CHROME_FLAGS {
        flags.push((*f).into());
    }
    for arg in extra_args {
        flags.push(arg.clone());
    }
    flags
}

const BASE_CHROME_FLAGS: &[&str] = &[
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
#[must_use]
pub fn detect_chromium() -> String {
    if let Ok(p) = std::env::var("CHROMIUM_PATH") {
        if std::path::Path::new(&p).exists() {
            return p;
        }
    }

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

    #[cfg(target_os = "macos")]
    {
        let bundles = [
            "Google Chrome.app/Contents/MacOS/Google Chrome",
            "Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
            "Chromium.app/Contents/MacOS/Chromium",
            "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ];
        for bundle in &bundles {
            let sys = std::path::PathBuf::from("/Applications").join(bundle);
            if sys.exists() {
                return sys.to_string_lossy().to_string();
            }
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
