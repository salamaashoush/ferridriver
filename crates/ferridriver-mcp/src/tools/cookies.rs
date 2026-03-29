use crate::params::CookiesParams;
use crate::server::{McpServer, sess};
use ferridriver::backend::CookieData;
use rmcp::{
  ErrorData,
  handler::server::wrapper::Parameters,
  model::{CallToolResult, Content},
  tool, tool_router,
};

#[tool_router(router = cookies_router, vis = "pub")]
impl McpServer {
  #[tool(
    name = "cookies",
    description = "Manage cookies. Actions: get (list all), set (create/update), delete (remove by name), clear (remove all)."
  )]
  async fn cookies(&self, Parameters(p): Parameters<CookiesParams>) -> Result<CallToolResult, ErrorData> {
    let s = sess(p.session.as_ref());
    let _guard = self.session_guard(s).await;
    let page = Box::pin(self.page(s)).await?;
    match p.action.as_str() {
      "get" => {
        let cookies = page.cookies().await.map_err(Self::err)?;
        let list: Vec<serde_json::Value> = cookies.iter().map(|c| {
                    serde_json::json!({"name": c.name, "value": c.value, "domain": c.domain, "path": c.path, "secure": c.secure, "httpOnly": c.http_only})
                }).collect();
        Ok(CallToolResult::success(vec![Content::text(
          serde_json::to_string_pretty(&list).unwrap_or_default(),
        )]))
      },
      "set" => {
        let name = p.name.as_deref().ok_or_else(|| Self::err("'name' required for set"))?;
        let value = p
          .value
          .as_deref()
          .ok_or_else(|| Self::err("'value' required for set"))?;
        let cookie = CookieData {
          name: name.to_string(),
          value: value.to_string(),
          domain: p.domain.clone().unwrap_or_default(),
          path: p.path.clone().unwrap_or_default(),
          secure: p.secure.unwrap_or(false),
          http_only: p.http_only.unwrap_or(false),
          expires: p.expires,
        };
        page.set_cookie(cookie).await.map_err(Self::err)?;
        Ok(CallToolResult::success(vec![Content::text(format!(
          "Cookie '{name}' set."
        ))]))
      },
      "delete" => {
        let name = p
          .name
          .as_deref()
          .ok_or_else(|| Self::err("'name' required for delete"))?;
        page.delete_cookie(name, p.domain.as_deref()).await.map_err(Self::err)?;
        Ok(CallToolResult::success(vec![Content::text(format!(
          "Cookie '{name}' deleted."
        ))]))
      },
      "clear" => {
        page.clear_cookies().await.map_err(Self::err)?;
        Ok(CallToolResult::success(vec![Content::text("Cookies cleared.")]))
      },
      other => Err(Self::err(format!(
        "Unknown action '{other}'. Use: get, set, delete, clear."
      ))),
    }
  }
}
