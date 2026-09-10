//! Typed WebMCP commands over the page's existing CDP session.

use std::sync::Arc;

use ferridriver::{FerriError, Page, Result};
use rquickjs::function::Opt;
use rquickjs::{Ctx, JsLifetime, Value, class::Trace};

use crate::bindings::convert::{FerriResultCtxExt, json_to_js, serde_from_js};

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "WebMCP")]
pub struct WebMcpJs {
  #[qjs(skip_trace)]
  page: Arc<Page>,
  #[qjs(skip_trace)]
  session: Arc<tokio::sync::Mutex<Option<ferridriver::CdpSession>>>,
}

impl Clone for WebMcpJs {
  fn clone(&self) -> Self {
    Self {
      page: self.page.clone(),
      session: self.session.clone(),
    }
  }
}

impl WebMcpJs {
  #[must_use]
  pub fn new(page: Arc<Page>) -> Self {
    Self {
      page,
      session: Arc::new(tokio::sync::Mutex::new(None)),
    }
  }

  async fn session(&self) -> Result<ferridriver::CdpSession> {
    let context = self
      .page
      .context()
      .ok_or_else(|| FerriError::unsupported("WebMCP requires a browser context"))?;
    let mut session = self.session.lock().await;
    if session.is_none() {
      *session = Some(context.new_cdp_session(&self.page).await?);
    }
    let Some(session) = session.as_ref() else {
      return Err(FerriError::Backend("WebMCP session initialization failed".to_string()));
    };
    Ok(session.clone())
  }

  async fn send(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    self.session().await?.send(method, params).await
  }

  fn frame_id(&self) -> String {
    self.page.main_frame().frame_id().to_string()
  }
}

#[rquickjs::methods]
impl WebMcpJs {
  #[qjs(rename = "listTools")]
  pub async fn list_tools<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
  ) -> rquickjs::Result<Value<'js>> {
    call_site
      .scope(async move {
        let session = self.session().await.into_js_with(&ctx)?;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let sender = std::sync::Mutex::new(Some(sender));
        let listener = session.once(
          "WebMCP.toolsAdded",
          std::sync::Arc::new(move |payload| {
            if let Ok(mut sender) = sender.lock()
              && let Some(sender) = sender.take()
            {
              let _ = sender.send(payload);
            }
          }),
        );
        session
          .send("WebMCP.enable", serde_json::json!({}))
          .await
          .into_js_with(&ctx)?;
        let payload = match tokio::time::timeout(std::time::Duration::from_millis(100), receiver).await {
          Ok(Ok(payload)) => payload,
          _ => {
            session.off(listener);
            return json_to_js(&ctx, &serde_json::json!([]));
          },
        };
        let tools = payload.get("tools").cloned().unwrap_or_else(|| serde_json::json!([]));
        json_to_js(&ctx, &tools)
      })
      .await
  }

  #[qjs(rename = "enable")]
  pub async fn enable<'js>(&self, call_site: crate::bindings::CallSite, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    call_site
      .scope(async move {
        let result = self
          .send("WebMCP.enable", serde_json::json!({}))
          .await
          .into_js_with(&ctx)?;
        json_to_js(&ctx, &result)
      })
      .await
  }

  #[qjs(rename = "disable")]
  pub async fn disable<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
  ) -> rquickjs::Result<Value<'js>> {
    call_site
      .scope(async move {
        let result = self
          .send("WebMCP.disable", serde_json::json!({}))
          .await
          .into_js_with(&ctx)?;
        json_to_js(&ctx, &result)
      })
      .await
  }

  #[qjs(rename = "invokeTool")]
  pub async fn invoke_tool<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    tool_name: String,
    input: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let input = match input.0 {
      Some(value) if !value.is_undefined() && !value.is_null() => serde_from_js(&ctx, value)?,
      _ => serde_json::Value::Object(serde_json::Map::new()),
    };
    call_site
      .scope(async move {
        let result = self
          .send(
            "WebMCP.invokeTool",
            serde_json::json!({ "frameId": self.frame_id(), "toolName": tool_name, "input": input }),
          )
          .await
          .into_js_with(&ctx)?;
        json_to_js(&ctx, &result)
      })
      .await
  }

  #[qjs(rename = "cancelInvocation")]
  pub async fn cancel_invocation<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    invocation_id: String,
  ) -> rquickjs::Result<Value<'js>> {
    call_site
      .scope(async move {
        let result = self
          .send(
            "WebMCP.cancelInvocation",
            serde_json::json!({ "frameId": self.frame_id(), "invocationId": invocation_id }),
          )
          .await
          .into_js_with(&ctx)?;
        json_to_js(&ctx, &result)
      })
      .await
  }
}
