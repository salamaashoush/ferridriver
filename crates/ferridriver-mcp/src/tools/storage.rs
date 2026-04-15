use crate::params::StorageParams;
use crate::server::{McpServer, sess};
use rmcp::{
  ErrorData,
  handler::server::wrapper::Parameters,
  model::{CallToolResult, Content},
  tool, tool_router,
};

#[tool_router(router = storage_router, vis = "pub")]
impl McpServer {
  #[tool(
    name = "storage",
    description = "Manage localStorage. Actions: get (read key), set (write key=value), list (all entries), clear (remove all)."
  )]
  async fn storage(&self, Parameters(p): Parameters<StorageParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_opt());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    match p.action.as_str() {
      "get" => {
        let key = p.key.as_deref().ok_or_else(|| Self::err("'key' required for get"))?;
        let r = page
          .evaluate(&format!("localStorage.getItem('{}')", key.replace('\'', "\\'")))
          .await
          .map_err(Self::err)?;
        Ok(CallToolResult::success(vec![Content::text(
          r.map_or("null".into(), |v| v.to_string()),
        )]))
      },
      "set" => {
        let key = p.key.as_deref().ok_or_else(|| Self::err("'key' required for set"))?;
        let value = p
          .value
          .as_deref()
          .ok_or_else(|| Self::err("'value' required for set"))?;
        page
          .evaluate(&format!(
            "localStorage.setItem('{}', '{}')",
            key.replace('\'', "\\'"),
            value.replace('\'', "\\'")
          ))
          .await
          .map_err(Self::err)?;
        Ok(CallToolResult::success(vec![Content::text(format!(
          "Set '{key}'='{value}'."
        ))]))
      },
      "list" => {
        let r = page
          .evaluate("JSON.stringify(Object.fromEntries(Object.entries(localStorage)))")
          .await
          .map_err(Self::err)?;
        let val = r.as_ref().and_then(|v| v.as_str()).unwrap_or("{}");
        let parsed: serde_json::Value = serde_json::from_str(val).unwrap_or_default();
        Ok(CallToolResult::success(vec![Content::text(
          serde_json::to_string_pretty(&parsed).unwrap_or_default(),
        )]))
      },
      "clear" => {
        page.evaluate("localStorage.clear()").await.map_err(Self::err)?;
        Ok(CallToolResult::success(vec![Content::text("localStorage cleared.")]))
      },
      other => Err(Self::err(format!(
        "Unknown action '{other}'. Use: get, set, list, clear."
      ))),
    }
  }
}
