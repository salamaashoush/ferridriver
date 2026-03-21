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
            page.find_element(sel)
                .await
                .map_err(|e| format!("Selector '{sel}' not found: {e}"))
        } else {
            Err("Provide 'ref' (from snapshot) or 'selector'.".into())
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
                .enable_resources_subscribe()
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
            let resources = vec![
                Annotated::new(RawResource {
                    uri: "browser://console".into(),
                    name: "Console Log".into(),
                    title: Some("Browser console messages".into()),
                    description: Some("Console log/warn/error messages from the active page".into()),
                    mime_type: Some("application/json".into()),
                    size: None, icons: None, meta: None,
                }, None),
                Annotated::new(RawResource {
                    uri: "browser://network".into(),
                    name: "Network Log".into(),
                    title: Some("Network requests".into()),
                    description: Some("HTTP requests made by the active page".into()),
                    mime_type: Some("application/json".into()),
                    size: None, icons: None, meta: None,
                }, None),
                Annotated::new(RawResource {
                    uri: "browser://snapshot".into(),
                    name: "Page Snapshot".into(),
                    title: Some("Accessibility tree snapshot".into()),
                    description: Some("Current page a11y tree with element refs".into()),
                    mime_type: Some("text/plain".into()),
                    size: None, icons: None, meta: None,
                }, None),
                Annotated::new(RawResource {
                    uri: "browser://screenshot".into(),
                    name: "Screenshot".into(),
                    title: Some("Page screenshot".into()),
                    description: Some("PNG screenshot of the current page".into()),
                    mime_type: Some("image/png".into()),
                    size: None, icons: None, meta: None,
                }, None),
                Annotated::new(RawResource {
                    uri: "browser://cookies".into(),
                    name: "Cookies".into(),
                    title: Some("Page cookies".into()),
                    description: Some("All cookies for the current page".into()),
                    mime_type: Some("application/json".into()),
                    size: None, icons: None, meta: None,
                }, None),
                Annotated::new(RawResource {
                    uri: "browser://page-info".into(),
                    name: "Page Info".into(),
                    title: Some("Current page URL and title".into()),
                    description: Some("URL, title, and session info".into()),
                    mime_type: Some("application/json".into()),
                    size: None, icons: None, meta: None,
                }, None),
            ];
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
            let mut state = self.state.lock().await;
            state.ensure_browser().await.map_err(|e| Self::err(e))?;
            let page = state.active_page("default").map_err(|e| Self::err(e))?.clone();

            match uri.as_str() {
                "browser://console" => {
                    let msgs = state.console_messages("default", None, 100).await
                        .map_err(|e| Self::err(e))?;
                    drop(state);
                    let json = serde_json::to_string_pretty(&msgs).unwrap_or_default();
                    Ok(ReadResourceResult::new(vec![
                        ResourceContents::text(json, uri).with_mime_type("application/json")
                    ]))
                }
                "browser://network" => {
                    let reqs = state.network_requests("default", 100).await
                        .map_err(|e| Self::err(e))?;
                    drop(state);
                    let json = serde_json::to_string_pretty(&reqs).unwrap_or_default();
                    Ok(ReadResourceResult::new(vec![
                        ResourceContents::text(json, uri).with_mime_type("application/json")
                    ]))
                }
                "browser://snapshot" => {
                    drop(state);
                    let snap = self.snap(&page, "default").await;
                    Ok(ReadResourceResult::new(vec![
                        ResourceContents::text(snap, uri).with_mime_type("text/plain")
                    ]))
                }
                "browser://screenshot" => {
                    drop(state);
                    let bytes = page.screenshot(
                        chromiumoxide::page::ScreenshotParams::builder().build()
                    ).await.map_err(|e| Self::err(format!("{e}")))?;
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    Ok(ReadResourceResult::new(vec![
                        ResourceContents::blob(b64, uri).with_mime_type("image/png")
                    ]))
                }
                "browser://cookies" => {
                    drop(state);
                    let cookies = page.get_cookies().await.map_err(|e| Self::err(format!("{e}")))?;
                    let list: Vec<serde_json::Value> = cookies.iter().map(|c| {
                        serde_json::json!({"name": c.name, "value": c.value, "domain": c.domain})
                    }).collect();
                    let json = serde_json::to_string_pretty(&list).unwrap_or_default();
                    Ok(ReadResourceResult::new(vec![
                        ResourceContents::text(json, uri).with_mime_type("application/json")
                    ]))
                }
                "browser://page-info" => {
                    drop(state);
                    let url = page.url().await.ok().flatten().unwrap_or_default();
                    let title = page.get_title().await.ok().flatten().unwrap_or_default();
                    let json = serde_json::to_string_pretty(&serde_json::json!({
                        "url": url, "title": title, "session": "default"
                    })).unwrap_or_default();
                    Ok(ReadResourceResult::new(vec![
                        ResourceContents::text(json, uri).with_mime_type("application/json")
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
