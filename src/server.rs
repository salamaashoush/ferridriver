//! ChromeyMcp server struct and shared helpers used by all tools.

use crate::snapshot;
use crate::state::{BrowserState, ConnectMode};
use chromiumoxide::cdp::browser_protocol::dom::{BackendNodeId, ResolveNodeParams};
use chromiumoxide::cdp::js_protocol::runtime::CallFunctionOnParams;
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
pub struct ChromeyMcp {
    pub(crate) state: State,
    pub(crate) tool_router: ToolRouter<Self>,
}

impl std::fmt::Debug for ChromeyMcp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChromeyMcp").finish()
    }
}

impl ChromeyMcp {
    pub fn new(mode: ConnectMode) -> Self {
        let state = Arc::new(Mutex::new(BrowserState::new(mode)));
        Self {
            state,
            tool_router: Self::combined_router(),
        }
    }

    pub fn err(msg: impl Into<String>) -> ErrorData {
        ErrorData::internal_error(msg.into(), None)
    }

    /// Get page for a session, ensuring browser is launched.
    pub async fn page(&self, session: &str) -> Result<chromiumoxide::Page, ErrorData> {
        let mut state = self.state.lock().await;
        state.ensure_browser().await.map_err(|e| Self::err(e))?;
        let page = state
            .active_page(session)
            .map_err(|e| Self::err(e))?
            .clone();
        Ok(page)
    }

    /// Resolve ref to element via CDP, or fall back to CSS selector.
    pub async fn resolve(
        page: &chromiumoxide::Page,
        ref_map: &std::collections::HashMap<String, i64>,
        r#ref: &Option<String>,
        selector: &Option<String>,
    ) -> Result<chromiumoxide::Element, String> {
        if let Some(r) = r#ref {
            let backend_id = ref_map
                .get(r)
                .ok_or_else(|| format!("Unknown ref '{r}'. Take a new snapshot."))?;

            let resolve = ResolveNodeParams::builder()
                .backend_node_id(BackendNodeId::new(*backend_id))
                .build();
            let resolved = page
                .execute(resolve)
                .await
                .map_err(|e| format!("Ref '{r}' stale: {e}"))?;
            let oid = resolved
                .result
                .object
                .object_id
                .ok_or_else(|| format!("Ref '{r}' no longer valid."))?;

            let tag = CallFunctionOnParams::builder()
                .object_id(oid)
                .function_declaration(format!(
                    "function() {{ this.setAttribute('data-cref', '{r}'); }}"
                ))
                .build()
                .map_err(|e| format!("Tag build error: {e}"))?;
            page.execute(tag)
                .await
                .map_err(|e| format!("Tag failed: {e}"))?;

            page.find_element(&format!("[data-cref='{r}']"))
                .await
                .map_err(|e| format!("Ref '{r}' element not found: {e}"))
        } else if let Some(sel) = selector {
            match page.find_element(sel).await {
                Ok(el) => Ok(el),
                Err(_) => {
                    // Try to suggest alternatives
                    let hint = Self::suggest_selectors(page, sel).await;
                    Err(format!("Selector '{sel}' not found.{hint}"))
                }
            }
        } else {
            Err("Provide 'ref' (from snapshot) or 'selector'.".into())
        }
    }

    /// When a selector fails, suggest what IS available on the page.
    async fn suggest_selectors(page: &chromiumoxide::Page, failed: &str) -> String {
        let js = r#"(function(){
            const ids = [...document.querySelectorAll('[id]')].slice(0,10).map(e => '#'+e.id);
            const inputs = [...document.querySelectorAll('input,button,select,textarea,a')]
                .slice(0,10).map(e => {
                    if (e.id) return '#'+e.id;
                    if (e.name) return e.tagName.toLowerCase()+'[name="'+e.name+'"]';
                    if (e.className) return e.tagName.toLowerCase()+'.'+e.className.split(' ')[0];
                    return e.tagName.toLowerCase();
                });
            return JSON.stringify({ids, inputs});
        })()"#;
        match page.evaluate(js).await {
            Ok(r) => {
                if let Some(val) = r.value().and_then(|v| v.as_str()) {
                    if let Ok(data) = serde_json::from_str::<serde_json::Value>(val) {
                        let ids: Vec<&str> = data["ids"].as_array()
                            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                            .unwrap_or_default();
                        let inputs: Vec<&str> = data["inputs"].as_array()
                            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                            .unwrap_or_default();
                        let mut suggestions = Vec::new();
                        if !ids.is_empty() {
                            suggestions.push(format!("IDs on page: {}", ids.join(", ")));
                        }
                        if !inputs.is_empty() {
                            suggestions.push(format!("Interactive: {}", inputs.join(", ")));
                        }
                        if !suggestions.is_empty() {
                            return format!(" Available: {}", suggestions.join(". "));
                        }
                    }
                }
                String::new()
            }
            Err(_) => String::new(),
        }
    }

    /// Build snapshot text and store ref_map for the session.
    pub async fn snap(&self, page: &chromiumoxide::Page, session: &str) -> String {
        match snapshot::page_context_with_snapshot(page).await {
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
        page: &chromiumoxide::Page,
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
impl ServerHandler for ChromeyMcp {
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

    // ── Logging ──────────────────────────────────────────────────────────

    fn set_level(
        &self,
        _request: SetLevelRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<(), ErrorData>> + Send + '_ {
        std::future::ready(Ok(()))
    }

    // ── Resources ────────────────────────────────────────────────────────

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
                Annotated::new(RawResource {
                    uri: uri.into(), name: name.into(), title: None,
                    description: Some(desc.into()), mime_type: Some(mime.into()),
                    size: None, icons: None, meta: None,
                }, None)
            };

            // Per-session resources (dynamic — grows as sessions are created)
            for sess in &sessions {
                let s = &sess.name;
                let url = sess.pages.iter().find(|p| p.active).map(|p| p.url.as_str()).unwrap_or("");
                let title = sess.pages.iter().find(|p| p.active).map(|p| p.title.as_str()).unwrap_or("");

                resources.push(res(
                    &format!("browser://session/{s}/page-info"),
                    &format!("[{s}] Page Info"),
                    &format!("{url} — {title}"),
                    "application/json",
                ));
                resources.push(res(
                    &format!("browser://session/{s}/snapshot"),
                    &format!("[{s}] Snapshot"),
                    &format!("A11y tree for session '{s}'"),
                    "text/plain",
                ));
                resources.push(res(
                    &format!("browser://session/{s}/screenshot"),
                    &format!("[{s}] Screenshot"),
                    &format!("PNG screenshot of session '{s}'"),
                    "image/png",
                ));
                resources.push(res(
                    &format!("browser://session/{s}/console"),
                    &format!("[{s}] Console"),
                    &format!("Console messages in session '{s}'"),
                    "application/json",
                ));
                resources.push(res(
                    &format!("browser://session/{s}/network"),
                    &format!("[{s}] Network"),
                    &format!("Network requests in session '{s}'"),
                    "application/json",
                ));
                resources.push(res(
                    &format!("browser://session/{s}/cookies"),
                    &format!("[{s}] Cookies"),
                    &format!("Cookies in session '{s}'"),
                    "application/json",
                ));
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

            // Parse URI: browser://session/{session}/{resource}
            // Also support legacy browser://{resource} → session=default
            let (session, resource) = if uri.starts_with("browser://session/") {
                let rest = &uri["browser://session/".len()..];
                let mut parts = rest.splitn(2, '/');
                let sess = parts.next().unwrap_or("default");
                let res = parts.next().unwrap_or("");
                (sess.to_string(), res.to_string())
            } else if uri.starts_with("browser://") {
                ("default".to_string(), uri["browser://".len()..].to_string())
            } else {
                return Err(Self::err(format!("Unknown resource URI: {uri}")));
            };

            let mut state = self.state.lock().await;
            state.ensure_browser().await.map_err(|e| Self::err(e))?;
            let page = state.active_page(&session).map_err(|e| Self::err(e))?.clone();

            match resource.as_str() {
                "page-info" => {
                    drop(state);
                    let url = page.url().await.ok().flatten().unwrap_or_default();
                    let title = page.get_title().await.ok().flatten().unwrap_or_default();
                    let json = serde_json::to_string_pretty(&serde_json::json!({
                        "url": url, "title": title, "session": session
                    })).unwrap_or_default();
                    Ok(ReadResourceResult::new(vec![
                        ResourceContents::text(json, uri).with_mime_type("application/json")
                    ]))
                }
                "console" => {
                    let msgs = state.console_messages(&session, None, 100).await
                        .map_err(|e| Self::err(e))?;
                    drop(state);
                    Ok(ReadResourceResult::new(vec![
                        ResourceContents::text(serde_json::to_string_pretty(&msgs).unwrap(), uri)
                            .with_mime_type("application/json")
                    ]))
                }
                "network" => {
                    let reqs = state.network_requests(&session, 100).await
                        .map_err(|e| Self::err(e))?;
                    drop(state);
                    Ok(ReadResourceResult::new(vec![
                        ResourceContents::text(serde_json::to_string_pretty(&reqs).unwrap(), uri)
                            .with_mime_type("application/json")
                    ]))
                }
                "snapshot" => {
                    drop(state);
                    let snap = self.snap(&page, &session).await;
                    Ok(ReadResourceResult::new(vec![
                        ResourceContents::text(snap, uri).with_mime_type("text/plain")
                    ]))
                }
                "screenshot" => {
                    drop(state);
                    let bytes = page.screenshot(
                        chromiumoxide::page::ScreenshotParams::builder().build()
                    ).await.map_err(|e| Self::err(format!("{e}")))?;
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    Ok(ReadResourceResult::new(vec![
                        ResourceContents::blob(b64, uri).with_mime_type("image/png")
                    ]))
                }
                "cookies" => {
                    drop(state);
                    let cookies = page.get_cookies().await.map_err(|e| Self::err(format!("{e}")))?;
                    let list: Vec<serde_json::Value> = cookies.iter().map(|c| {
                        serde_json::json!({"name": c.name, "value": c.value, "domain": c.domain})
                    }).collect();
                    Ok(ReadResourceResult::new(vec![
                        ResourceContents::text(serde_json::to_string_pretty(&list).unwrap(), uri)
                            .with_mime_type("application/json")
                    ]))
                }
                _ => Err(Self::err(format!("Unknown resource: {uri}")))
            }
        }
    }

    // ── Prompts ──────────────────────────────────────────────────────────

    fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListPromptsResult, ErrorData>> + Send + '_ {
        async {
            let prompts = vec![
                Prompt::new(
                    "debug-page",
                    Some("Analyze the page for errors, broken elements, and console issues"),
                    Some(vec![PromptArgument::new("url").with_description("URL to debug").with_required(false)]),
                ),
                Prompt::new(
                    "test-form",
                    Some("Fill and submit a form, verify the result"),
                    Some(vec![
                        PromptArgument::new("url").with_description("Page URL with the form").with_required(true),
                        PromptArgument::new("submit_selector").with_description("Submit button selector").with_required(false),
                    ]),
                ),
                Prompt::new(
                    "audit-accessibility",
                    Some("Check page accessibility using the a11y tree"),
                    Some(vec![PromptArgument::new("url").with_description("URL to audit").with_required(true)]),
                ),
                Prompt::new(
                    "compare-sessions",
                    Some("Compare page state between two browser sessions"),
                    Some(vec![
                        PromptArgument::new("url").with_description("URL to compare").with_required(true),
                        PromptArgument::new("session_a").with_description("First session").with_required(true),
                        PromptArgument::new("session_b").with_description("Second session").with_required(true),
                    ]),
                ),
            ];
            let mut result = ListPromptsResult::default();
            result.prompts = prompts;
            Ok(result)
        }
    }

    fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<GetPromptResult, ErrorData>> + Send + '_ {
        async move {
            let args = request.arguments.unwrap_or_default();
            let get_arg = |key: &str| -> String {
                args.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
            };
            let url = get_arg("url");

            match request.name.as_str() {
                "debug-page" => {
                    let nav = if url.is_empty() { String::new() } else { format!("First navigate to {url}.\n") };
                    Ok(GetPromptResult::new(vec![
                        PromptMessage::new_text(PromptMessageRole::User, format!(
                            "{nav}Debug the current page:\n\
                             1. Take a snapshot to understand the page structure\n\
                             2. Check console_messages for errors\n\
                             3. Check network_requests for failed requests (4xx/5xx)\n\
                             4. Report all issues found with suggested fixes"
                        )),
                    ]))
                }
                "test-form" => {
                    let submit = { let s = get_arg("submit_selector"); if s.is_empty() { "the submit button".into() } else { s } };
                    Ok(GetPromptResult::new(vec![
                        PromptMessage::new_text(PromptMessageRole::User, format!(
                            "Test the form on {url}:\n\
                             1. Navigate to the page\n\
                             2. Take a snapshot to identify form fields\n\
                             3. Fill all required fields with realistic test data\n\
                             4. Click {submit}\n\
                             5. Verify the form submitted successfully (check URL change, success message, or network request)\n\
                             6. Report the result"
                        )),
                    ]))
                }
                "audit-accessibility" => {
                    Ok(GetPromptResult::new(vec![
                        PromptMessage::new_text(PromptMessageRole::User, format!(
                            "Audit the accessibility of {url}:\n\
                             1. Navigate to the page\n\
                             2. Take a snapshot (a11y tree)\n\
                             3. Check for: missing labels, incorrect heading hierarchy, images without alt text, \
                                interactive elements without accessible names, form inputs without labels\n\
                             4. Report issues with severity and how to fix each one"
                        )),
                    ]))
                }
                "compare-sessions" => {
                    let sa = { let s = get_arg("session_a"); if s.is_empty() { "userA".into() } else { s } };
                    let sb = { let s = get_arg("session_b"); if s.is_empty() { "userB".into() } else { s } };
                    Ok(GetPromptResult::new(vec![
                        PromptMessage::new_text(PromptMessageRole::User, format!(
                            "Compare {url} between two sessions:\n\
                             1. Open the page in session='{sa}' and session='{sb}'\n\
                             2. Take a snapshot of each\n\
                             3. Compare: visible content differences, available navigation, form fields, cookies\n\
                             4. Report what differs between the two sessions"
                        )),
                    ]))
                }
                _ => Err(Self::err(format!("Unknown prompt: {}", request.name)))
            }
        }
    }

}
