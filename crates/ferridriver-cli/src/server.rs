//! McpServer server struct and shared helpers used by all tools.

use std::collections::HashMap;
use ferridriver::backend::{AnyElement, AnyPage};
use ferridriver::snapshot;
use ferridriver::state::{BrowserState, ConnectMode};
use ferridriver::backend::BackendKind;
use ferridriver::actions;
use ferridriver::Page;
use base64::Engine;
use rmcp::{
    handler::server::router::tool::ToolRouter,
    model::*,
    service::RequestContext,
    tool_handler, ErrorData, RoleServer, ServerHandler,
};
use std::sync::Arc;
use tokio::sync::Mutex;

pub type State = Arc<Mutex<BrowserState>>;

pub fn sess(s: &Option<String>) -> &str {
    s.as_deref().unwrap_or("default")
}

#[derive(Clone)]
pub struct McpServer {
    pub(crate) state: State,
    pub(crate) tool_router: ToolRouter<Self>,
    pub(crate) session_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl std::fmt::Debug for McpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServer").finish()
    }
}

impl McpServer {
    pub fn new(mode: ConnectMode, backend: BackendKind) -> Self {
        let state = Arc::new(Mutex::new(BrowserState::new(mode, backend)));
        Self {
            state,
            tool_router: Self::combined_router(),
            session_locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn err(msg: impl Into<String>) -> ErrorData {
        ErrorData::internal_error(msg.into(), None)
    }

    pub async fn session_guard(&self, session: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.session_locks.lock().await;
            locks.entry(session.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        lock.lock_owned().await
    }

    /// Get a Page for a session, ensuring browser is launched.
    pub async fn page(&self, session: &str) -> Result<Page, ErrorData> {
        let mut state = self.state.lock().await;
        state.ensure_browser().await.map_err(|e| Self::err(e))?;
        let any_page = state
            .active_page(session)
            .map_err(|e| Self::err(e))?
            .clone();
        Ok(Page::new(any_page))
    }

    /// Get raw AnyPage (for low-level ops that Page doesn't cover yet).
    pub async fn raw_page(&self, session: &str) -> Result<AnyPage, ErrorData> {
        let mut state = self.state.lock().await;
        state.ensure_browser().await.map_err(|e| Self::err(e))?;
        let page = state.active_page(session).map_err(|e| Self::err(e))?.clone();
        Ok(page)
    }

    /// Resolve ref to element -- delegates to actions::resolve_element.
    pub async fn resolve(
        page: &Page,
        ref_map: &HashMap<String, i64>,
        r#ref: &Option<String>,
        selector: &Option<String>,
    ) -> Result<AnyElement, String> {
        actions::resolve_element(page.inner(), ref_map, r#ref.as_deref(), selector.as_deref()).await
    }

    /// Build snapshot text and store ref_map for the session.
    pub async fn snap(&self, page: &Page, session: &str) -> String {
        match snapshot::page_context_with_snapshot(page.inner()).await {
            Ok((text, ref_map)) => {
                if let Ok(mut state) = self.state.try_lock() {
                    state.set_ref_map(session, ref_map);
                }
                text
            }
            Err(e) => format!("\n[snapshot error: {e}]"),
        }
    }

    /// Action result: description + auto-snapshot.
    pub async fn action_ok(
        &self,
        page: &Page,
        session: &str,
        msg: &str,
    ) -> Result<CallToolResult, ErrorData> {
        let snap = self.snap(page, session).await;
        Ok(CallToolResult::success(vec![Content::text(format!(
            "{msg}\n\n{snap}"
        ))]))
    }
}

#[tool_handler]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .enable_logging()
                .build(),
        )
        .with_instructions(
            "Browser automation. All tools accept optional 'session' param (default: 'default'). \
             Different sessions have isolated cookies/storage — use for multi-user testing.\n\
             Actions return an accessibility snapshot with [ref=eN] identifiers. \
             Use these refs with click/hover/fill. Prefer snapshot over screenshot."
                .to_string(),
        )
    }

    fn set_level(
        &self,
        _request: SetLevelRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<(), ErrorData>> + Send + '_ {
        std::future::ready(Ok(()))
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListResourcesResult, ErrorData>> + Send + '_ {
        async {
            let state = self.state.lock().await;
            let sessions = state.list_sessions().await;
            drop(state);

            let mut resources = Vec::new();
            let res = |uri: &str, name: &str, desc: &str, mime: &str| -> Resource {
                Annotated::new(RawResource { uri: uri.into(), name: name.into(), title: None, description: Some(desc.into()), mime_type: Some(mime.into()), size: None, icons: None, meta: None }, None)
            };

            for sess in &sessions {
                let s = &sess.name;
                let url = sess.pages.iter().find(|p| p.active).map(|p| p.url.as_str()).unwrap_or("");
                let title = sess.pages.iter().find(|p| p.active).map(|p| p.title.as_str()).unwrap_or("");
                resources.push(res(&format!("browser://session/{s}/page-info"), &format!("[{s}] Page Info"), &format!("{url} — {title}"), "application/json"));
                resources.push(res(&format!("browser://session/{s}/snapshot"), &format!("[{s}] Snapshot"), &format!("A11y tree for session '{s}'"), "text/plain"));
                resources.push(res(&format!("browser://session/{s}/screenshot"), &format!("[{s}] Screenshot"), &format!("PNG screenshot of session '{s}'"), "image/png"));
                resources.push(res(&format!("browser://session/{s}/console"), &format!("[{s}] Console"), &format!("Console messages in session '{s}'"), "application/json"));
                resources.push(res(&format!("browser://session/{s}/network"), &format!("[{s}] Network"), &format!("Network requests in session '{s}'"), "application/json"));
                resources.push(res(&format!("browser://session/{s}/cookies"), &format!("[{s}] Cookies"), &format!("Cookies in session '{s}'"), "application/json"));
            }

            let mut result = ListResourcesResult::default();
            result.resources = resources;
            Ok(result)
        }
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ReadResourceResult, ErrorData>> + Send + '_ {
        async move {
            let uri = &request.uri;
            let (session, resource) = if uri.starts_with("browser://session/") {
                let rest = &uri["browser://session/".len()..];
                let mut parts = rest.splitn(2, '/');
                (parts.next().unwrap_or("default").to_string(), parts.next().unwrap_or("").to_string())
            } else if uri.starts_with("browser://") {
                ("default".to_string(), uri["browser://".len()..].to_string())
            } else {
                return Err(Self::err(format!("Unknown resource URI: {uri}")));
            };

            let page = self.page(&session).await?;

            match resource.as_str() {
                "page-info" => {
                    let url = page.url().await.unwrap_or_default();
                    let title = page.title().await.unwrap_or_default();
                    let json = serde_json::to_string_pretty(&serde_json::json!({"url": url, "title": title, "session": session})).unwrap_or_default();
                    Ok(ReadResourceResult::new(vec![ResourceContents::text(json, uri).with_mime_type("application/json")]))
                }
                "console" => {
                    let state = self.state.lock().await;
                    let msgs = state.console_messages(&session, None, 100).await.map_err(|e| Self::err(e))?;
                    drop(state);
                    Ok(ReadResourceResult::new(vec![ResourceContents::text(serde_json::to_string_pretty(&msgs).unwrap(), uri).with_mime_type("application/json")]))
                }
                "network" => {
                    let state = self.state.lock().await;
                    let reqs = state.network_requests(&session, 100).await.map_err(|e| Self::err(e))?;
                    drop(state);
                    Ok(ReadResourceResult::new(vec![ResourceContents::text(serde_json::to_string_pretty(&reqs).unwrap(), uri).with_mime_type("application/json")]))
                }
                "snapshot" => {
                    let snap = self.snap(&page, &session).await;
                    Ok(ReadResourceResult::new(vec![ResourceContents::text(snap, uri).with_mime_type("text/plain")]))
                }
                "screenshot" => {
                    let bytes = page.screenshot(ferridriver::options::ScreenshotOptions::default()).await.map_err(|e| Self::err(e))?;
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    Ok(ReadResourceResult::new(vec![ResourceContents::blob(b64, uri).with_mime_type("image/png")]))
                }
                "cookies" => {
                    let cookies = page.cookies().await.map_err(|e| Self::err(e))?;
                    let list: Vec<serde_json::Value> = cookies.iter().map(|c| serde_json::json!({"name": c.name, "value": c.value, "domain": c.domain})).collect();
                    Ok(ReadResourceResult::new(vec![ResourceContents::text(serde_json::to_string_pretty(&list).unwrap(), uri).with_mime_type("application/json")]))
                }
                _ => Err(Self::err(format!("Unknown resource: {uri}"))),
            }
        }
    }

    fn list_prompts(&self, _request: Option<PaginatedRequestParams>, _context: RequestContext<RoleServer>) -> impl std::future::Future<Output = Result<ListPromptsResult, ErrorData>> + Send + '_ {
        async {
            let prompts = vec![
                Prompt::new("debug-page", Some("Analyze the page for errors, broken elements, and console issues"), Some(vec![PromptArgument::new("url").with_description("URL to debug").with_required(false)])),
                Prompt::new("test-form", Some("Fill and submit a form, verify the result"), Some(vec![PromptArgument::new("url").with_description("Page URL with the form").with_required(true), PromptArgument::new("submit_selector").with_description("Submit button selector").with_required(false)])),
                Prompt::new("audit-accessibility", Some("Check page accessibility using the a11y tree"), Some(vec![PromptArgument::new("url").with_description("URL to audit").with_required(true)])),
                Prompt::new("compare-sessions", Some("Compare page state between two browser sessions"), Some(vec![PromptArgument::new("url").with_description("URL to compare").with_required(true), PromptArgument::new("session_a").with_description("First session").with_required(true), PromptArgument::new("session_b").with_description("Second session").with_required(true)])),
            ];
            let mut result = ListPromptsResult::default();
            result.prompts = prompts;
            Ok(result)
        }
    }

    fn get_prompt(&self, request: GetPromptRequestParams, _context: RequestContext<RoleServer>) -> impl std::future::Future<Output = Result<GetPromptResult, ErrorData>> + Send + '_ {
        async move {
            let args = request.arguments.unwrap_or_default();
            let get_arg = |key: &str| -> String { args.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string() };
            let url = get_arg("url");

            match request.name.as_str() {
                "debug-page" => {
                    let nav = if url.is_empty() { String::new() } else { format!("First navigate to {url}.\n") };
                    Ok(GetPromptResult::new(vec![PromptMessage::new_text(PromptMessageRole::User, format!("{nav}Debug the current page:\n1. Take a snapshot to understand the page structure\n2. Check console_messages for errors\n3. Check network_requests for failed requests (4xx/5xx)\n4. Report all issues found with suggested fixes"))]))
                }
                "test-form" => {
                    let submit = { let s = get_arg("submit_selector"); if s.is_empty() { "the submit button".into() } else { s } };
                    Ok(GetPromptResult::new(vec![PromptMessage::new_text(PromptMessageRole::User, format!("Test the form on {url}:\n1. Navigate to the page\n2. Take a snapshot to identify form fields\n3. Fill all required fields with realistic test data\n4. Click {submit}\n5. Verify the form submitted successfully\n6. Report the result"))]))
                }
                "audit-accessibility" => Ok(GetPromptResult::new(vec![PromptMessage::new_text(PromptMessageRole::User, format!("Audit the accessibility of {url}:\n1. Navigate to the page\n2. Take a snapshot (a11y tree)\n3. Check for: missing labels, incorrect heading hierarchy, images without alt text, interactive elements without accessible names, form inputs without labels\n4. Report issues with severity and how to fix each one"))])),
                "compare-sessions" => {
                    let sa = { let s = get_arg("session_a"); if s.is_empty() { "userA".into() } else { s } };
                    let sb = { let s = get_arg("session_b"); if s.is_empty() { "userB".into() } else { s } };
                    Ok(GetPromptResult::new(vec![PromptMessage::new_text(PromptMessageRole::User, format!("Compare {url} between two sessions:\n1. Open the page in session='{sa}' and session='{sb}'\n2. Take a snapshot of each\n3. Compare: visible content differences, available navigation, form fields, cookies\n4. Report what differs between the two sessions"))]))
                }
                _ => Err(Self::err(format!("Unknown prompt: {}", request.name))),
            }
        }
    }
}
