use napi_derive::napi;

use crate::error::IntoNapi;

#[napi(object)]
pub struct WebMcpOptions {
  pub timeout: Option<u32>,
}

fn lower_options(options: Option<WebMcpOptions>) -> ferridriver::web_mcp::WebMcpOptions {
  ferridriver::web_mcp::WebMcpOptions {
    timeout: options.and_then(|options| options.timeout.map(u64::from)),
  }
}

#[napi(js_name = "WebMCP")]
pub struct WebMcp {
  inner: ferridriver::web_mcp::WebMcp,
}

impl WebMcp {
  pub(crate) fn wrap(inner: ferridriver::web_mcp::WebMcp) -> Self {
    Self { inner }
  }
}

#[napi]
impl WebMcp {
  #[napi(
    ts_args_type = "options?: { timeout?: number }",
    ts_return_type = "Promise<Array<{ name: string; description: string; inputSchema?: unknown; annotations?: { readOnly?: boolean; untrustedContent?: boolean; consequential?: boolean } }>>"
  )]
  pub async fn tools(&self, options: Option<WebMcpOptions>) -> napi::Result<serde_json::Value> {
    let tools = self.inner.tools(lower_options(options)).await.into_napi()?;
    serde_json::to_value(tools).map_err(|error| napi::Error::from_reason(error.to_string()))
  }

  #[napi(
    ts_args_type = "name: string, input?: unknown, options?: { timeout?: number }",
    ts_return_type = "Promise<unknown>"
  )]
  pub async fn call_tool(
    &self,
    name: String,
    input: Option<crate::types::NapiEvaluateArg>,
    options: Option<WebMcpOptions>,
  ) -> napi::Result<crate::serialize_out::Evaluated> {
    let result = self
      .inner
      .call_tool(
        &name,
        crate::page::build_serialized_argument(input),
        lower_options(options),
      )
      .await
      .into_napi()?;
    Ok(crate::serialize_out::Evaluated(result))
  }
}
