use rquickjs::{Ctx, JsLifetime, Value, class::Trace, function::Opt};

use crate::bindings::convert::{
  FerriResultCtxExt, json_to_js, quickjs_arg_to_serialized, serde_from_js, serialized_value_to_quickjs,
};

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "WebMCP")]
pub struct WebMcpJs {
  #[qjs(skip_trace)]
  inner: ferridriver::web_mcp::WebMcp,
}

impl WebMcpJs {
  pub(crate) fn new(inner: ferridriver::web_mcp::WebMcp) -> Self {
    Self { inner }
  }
}

#[rquickjs::methods]
impl WebMcpJs {
  /// The tools the page registered over `WebMCP`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, or with the
  /// core error under its Playwright name when listing fails.
  pub async fn tools<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let options = options
      .0
      .filter(|value| !value.is_undefined() && !value.is_null())
      .map(|value| serde_from_js(&ctx, value))
      .transpose()?
      .unwrap_or_default();
    call_site
      .scope(async move {
        let tools = self.inner.tools(options).await.into_js_with(&ctx)?;
        let value =
          serde_json::to_value(tools).map_err(|error| rquickjs::Exception::throw_type(&ctx, &error.to_string()))?;
        json_to_js(&ctx, &value)
      })
      .await
  }

  /// Call the page's `WebMCP` tool `name` with `input`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for an `input` that cannot be serialized or a
  /// malformed `options` bag, or with the core error when the call fails.
  #[qjs(rename = "callTool")]
  pub async fn call_tool<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    name: String,
    input: Opt<Value<'js>>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let input = quickjs_arg_to_serialized(&ctx, input.0)?;
    let options = options
      .0
      .filter(|value| !value.is_undefined() && !value.is_null())
      .map(|value| serde_from_js(&ctx, value))
      .transpose()?
      .unwrap_or_default();
    call_site
      .scope(async move {
        let result = self.inner.call_tool(&name, input, options).await.into_js_with(&ctx)?;
        serialized_value_to_quickjs(&ctx, &result)
      })
      .await
  }

  /// Wait until the page registers the `WebMCP` tool `name`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, or with the
  /// core error (`TimeoutError` when the wait runs out).
  #[qjs(rename = "waitForTool")]
  pub async fn wait_for_tool<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    name: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let options = options
      .0
      .filter(|value| !value.is_undefined() && !value.is_null())
      .map(|value| serde_from_js(&ctx, value))
      .transpose()?
      .unwrap_or_default();
    call_site
      .scope(async move {
        let tool = self.inner.wait_for_tool(&name, options).await.into_js_with(&ctx)?;
        let value =
          serde_json::to_value(tool).map_err(|error| rquickjs::Exception::throw_type(&ctx, &error.to_string()))?;
        json_to_js(&ctx, &value)
      })
      .await
  }
}
