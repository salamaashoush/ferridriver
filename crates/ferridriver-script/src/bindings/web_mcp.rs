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
