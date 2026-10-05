use serde::{Deserialize, Serialize};

use crate::Frame;
use crate::backend::AnyPage;
use crate::backend::cdp::transport::CdpTransport;
use crate::error::{FerriError, Result};
use crate::protocol::{SerializedArgument, SerializedValue, argument_from_serde, result_to_serde};

mod classic;

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WebMcpOptions {
  pub timeout: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebMcpAnnotations {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub read_only: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub untrusted_content: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub consequential: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebMcpTool {
  pub name: String,
  pub description: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub input_schema: Option<serde_json::Value>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub annotations: Option<WebMcpAnnotations>,
}

#[derive(Clone)]
pub struct WebMcp {
  frame: Frame,
}

impl WebMcp {
  pub(crate) fn new(frame: Frame) -> Self {
    Self { frame }
  }

  /// # Errors
  /// Returns `Unsupported` when native discovery is unavailable, or a navigation,
  /// evaluation or timeout error when the frame cannot supply its catalogue.
  pub async fn tools(&self, options: WebMcpOptions) -> Result<Vec<WebMcpTool>> {
    self
      .frame
      .page()
      .traced_as("WebMCP", "tools", serde_json::json!({}), async {
        let result = self.run(None, argument_from_serde(&())?, options).await?;
        result_to_serde(&result)
      })
      .await
  }

  /// # Errors
  /// Missing tools, thrown tool exceptions, closed frames and deadlines reject
  /// the call. A tool result containing `isError: true` remains a result.
  pub async fn call_tool(
    &self,
    name: &str,
    input: SerializedArgument,
    options: WebMcpOptions,
  ) -> Result<SerializedValue> {
    self
      .frame
      .page()
      .traced_as(
        "WebMCP",
        "callTool",
        serde_json::json!({"name":name}),
        self.run(Some(name), input, options),
      )
      .await
  }

  async fn run(
    &self,
    name: Option<&str>,
    input: SerializedArgument,
    options: WebMcpOptions,
  ) -> Result<SerializedValue> {
    if self.frame.is_detached() {
      return Err(FerriError::target_closed(Some("WebMCP frame is detached".into())));
    }
    let timeout = options.timeout.unwrap_or_else(|| self.frame.page().default_timeout());
    let budget = crate::operation_budget::OperationBudget::operation(timeout)?;
    let evaluate = async {
      let firefox = match self.frame.page().inner() {
        AnyPage::Bidi(page) => page.session.browser_name.eq_ignore_ascii_case("firefox"),
        AnyPage::WebDriver(page) => page.capabilities["browserName"]
          .as_str()
          .is_some_and(|name| name.eq_ignore_ascii_case("firefox")),
        _ => false,
      };
      if let Some(name) = name
        && firefox
      {
        let source = format!(
          "async input => {{ const toolName = null; const legacyInput = false; {} }}",
          include_str!("web_mcp.js")
        );
        let discovered = self.evaluate(&source, argument_from_serde(&())?, true).await?;
        let tools: Vec<WebMcpTool> = result_to_serde(&operation_result(discovered)?)?;
        if !tools.iter().any(|tool| tool.name == name) {
          let available = tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>();
          let detail = if available.is_empty() {
            " The frame does not register any WebMCP tools.".to_owned()
          } else {
            format!(" Available tools: {}.", available.join(", "))
          };
          return Err(FerriError::evaluation(format!(
            "No WebMCP tool named \"{name}\".{detail}"
          )));
        }
      }
      let legacy_input = name.is_some() && self.legacy_chromium_input().await?;
      let source = format!(
        "async input => {{ const toolName = {}; const legacyInput = {legacy_input}; {} }}",
        serde_json::to_string(&name)?,
        include_str!("web_mcp.js")
      );
      if name.is_some() && matches!(self.frame.page().inner(), AnyPage::WebDriver(_)) {
        Box::pin(classic::call(&self.frame, &source, input)).await
      } else {
        self.evaluate(&source, input, !firefox || name.is_none()).await
      }
    };
    let result = budget
      .scope(budget.wait(evaluate))
      .await
      .map_err(|error| error.error("WebMCP operation"))??;
    operation_result(result)
  }

  async fn evaluate(&self, source: &str, input: SerializedArgument, isolated: bool) -> Result<SerializedValue> {
    if isolated {
      match self.frame.page().inner() {
        AnyPage::CdpPipe(page) => return page.evaluate_isolated(source, &input, self.frame.frame_id()).await,
        AnyPage::CdpWs(page) => return page.evaluate_isolated(source, &input, self.frame.frame_id()).await,
        AnyPage::Bidi(page) => return page.evaluate_isolated(source, &input, self.frame.frame_id()).await,
        _ => {},
      }
    }
    self.frame.evaluate_untraced(source, input, Some(true)).await
  }

  async fn legacy_chromium_input(&self) -> Result<bool> {
    let page = self.frame.page();
    let version = match page.inner() {
      AnyPage::CdpPipe(backend) => match page.context().and_then(|context| context.browser()) {
        Some(browser) => browser.version().to_owned(),
        None => cdp_version(&*backend.session_parts().0).await?,
      },
      AnyPage::CdpWs(backend) => match page.context().and_then(|context| context.browser()) {
        Some(browser) => browser.version().to_owned(),
        None => cdp_version(&*backend.session_parts().0).await?,
      },
      AnyPage::Bidi(backend) if is_chromium(&backend.session.browser_name) => backend.session.browser_version.clone(),
      AnyPage::WebDriver(backend) if backend.capabilities["browserName"].as_str().is_some_and(is_chromium) => backend
        .capabilities["browserVersion"]
        .as_str()
        .unwrap_or_default()
        .to_owned(),
      _ => return Ok(false),
    };
    let major = version
      .rsplit('/')
      .next()
      .and_then(|version| version.split('.').next())
      .and_then(|major| major.parse::<u32>().ok());
    major
      .map(|major| major < 155)
      .ok_or_else(|| FerriError::protocol("WebMCP", "missing Chromium version for native tool input encoding"))
  }
}

fn operation_result(result: SerializedValue) -> Result<SerializedValue> {
  match result {
    SerializedValue::Array { mut items, .. }
      if items.len() == 2 && items.first() == Some(&SerializedValue::Bool(true)) =>
    {
      items
        .pop()
        .ok_or_else(|| FerriError::protocol("WebMCP", "missing operation result"))
    },
    SerializedValue::Array { items, .. } if items.first() == Some(&SerializedValue::Bool(false)) => {
      Err(FerriError::unsupported(
        "WebMCP discovery is unavailable in this document; enable the browser's native WebMCP testing API",
      ))
    },
    _ => Err(FerriError::protocol("WebMCP", "invalid operation result")),
  }
}

fn is_chromium(name: &str) -> bool {
  name.eq_ignore_ascii_case("chrome") || name.eq_ignore_ascii_case("chromium")
}

async fn cdp_version(transport: &impl CdpTransport) -> Result<String> {
  let metadata = transport
    .send_command(None, "Browser.getVersion", &serde_json::json!({}))
    .await?;
  metadata["product"]
    .as_str()
    .map(str::to_owned)
    .ok_or_else(|| FerriError::protocol("Browser.getVersion", "missing product"))
}
