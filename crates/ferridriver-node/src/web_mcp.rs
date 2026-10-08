use napi_derive::napi;

use crate::error::IntoNapi;

#[napi(object)]
pub struct WebMcpOptions {
  pub timeout: Option<u32>,
  /// Also reach the tools of descendant frames, not only this frame's.
  pub all_frames: Option<bool>,
}

fn lower_options(options: Option<WebMcpOptions>) -> ferridriver::web_mcp::WebMcpOptions {
  let options = options.unwrap_or(WebMcpOptions {
    timeout: None,
    all_frames: None,
  });
  ferridriver::web_mcp::WebMcpOptions {
    timeout: options.timeout.map(u64::from),
    all_frames: options.all_frames.unwrap_or(false),
  }
}

/// Hints a page attaches to a WebMCP tool. Only hints that are set appear.
#[napi(object)]
pub struct WebMcpAnnotations {
  pub read_only: Option<bool>,
  pub untrusted_content: Option<bool>,
  pub consequential: Option<bool>,
  pub debugging: Option<bool>,
  /// A declarative form tool that submits itself instead of waiting for the user.
  pub autosubmit: Option<bool>,
}

/// The frame that registered a tool, as it was when the tools were listed.
#[napi(object)]
pub struct WebMcpFrame {
  pub name: String,
  pub url: String,
}

/// A tool registered through `document.modelContext` or a `<form toolname>`.
#[napi(object)]
pub struct WebMcpTool {
  pub name: String,
  pub title: Option<String>,
  pub description: String,
  pub input_schema: Option<serde_json::Value>,
  pub annotations: Option<WebMcpAnnotations>,
  /// The origin of the document that registered the tool.
  pub origin: Option<String>,
  /// Registered by a `<form toolname>` rather than by script.
  pub declarative: Option<bool>,
  pub frame: WebMcpFrame,
}

impl From<ferridriver::web_mcp::WebMcpTool> for WebMcpTool {
  fn from(tool: ferridriver::web_mcp::WebMcpTool) -> Self {
    Self {
      name: tool.name,
      title: tool.title,
      description: tool.description,
      input_schema: tool.input_schema,
      annotations: tool.annotations.map(|hints| WebMcpAnnotations {
        read_only: hints.read_only,
        untrusted_content: hints.untrusted_content,
        consequential: hints.consequential,
        debugging: hints.debugging,
        autosubmit: hints.autosubmit,
      }),
      origin: tool.origin,
      declarative: tool.declarative.then_some(true),
      frame: WebMcpFrame {
        name: tool.frame.name,
        url: tool.frame.url,
      },
    }
  }
}

/// The WebMCP tools a frame registers. A call that outlives its timeout is
/// canceled in the page: the tool's `AbortSignal` aborts.
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
  #[napi]
  pub async fn tools(&self, options: Option<WebMcpOptions>) -> napi::Result<Vec<WebMcpTool>> {
    let tools = self.inner.tools(lower_options(options)).await.into_napi()?;
    Ok(tools.into_iter().map(WebMcpTool::from).collect())
  }

  #[napi(
    ts_args_type = "name: string, input?: Record<string, unknown>, options?: WebMcpOptions",
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

  /// Resolve once a tool named `name` is registered.
  #[napi]
  pub async fn wait_for_tool(&self, name: String, options: Option<WebMcpOptions>) -> napi::Result<WebMcpTool> {
    let tool = self
      .inner
      .wait_for_tool(&name, lower_options(options))
      .await
      .into_napi()?;
    Ok(tool.into())
  }
}
