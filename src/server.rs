//! ChromeyMcp server struct and shared helpers used by all tools.

use crate::snapshot;
use crate::state::{BrowserState, ConnectMode};
use chromiumoxide::cdp::browser_protocol::dom::{BackendNodeId, ResolveNodeParams};
use chromiumoxide::cdp::js_protocol::runtime::CallFunctionOnParams;
use rmcp::{
    handler::server::router::tool::ToolRouter,
    model::*,
    tool_handler, ErrorData, ServerHandler,
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
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "Browser automation. All tools accept optional 'session' param (default: 'default'). \
                 Different sessions have isolated cookies/storage — use for multi-user testing.\n\
                 Actions return an accessibility snapshot with [ref=eN] identifiers. \
                 Use these refs with click/hover/fill. Prefer snapshot over screenshot."
                    .to_string(),
            )
    }
}
