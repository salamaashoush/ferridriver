//! `WebMCP`: the tools a page registers through `document.modelContext`.
//!
//! Discovery runs the standard `getTools()` in each frame, outside the page's
//! main world where the backend has one, so every backend reports the same
//! catalogue. A call goes through Chromium's `DevTools` `WebMCP` domain when the
//! browser exposes it, which reports the tool's own error and cancels the
//! invocation in the page when the caller gives up. Every other path runs the
//! standard `executeTool()` with an `AbortSignal` the page aborts at the same
//! deadline.

use serde::{Deserialize, Serialize};

use crate::Frame;
use crate::backend::AnyPage;
use crate::backend::cdp::transport::CdpTransport;
use crate::error::{FerriError, Result};
use crate::events::PageEvent;
use crate::protocol::{SerializedArgument, SerializedValue, argument_from_serde, result_to_serde};

mod classic;

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WebMcpOptions {
  pub timeout: Option<u64>,
  /// Also reach the tools of this frame's descendant frames.
  pub all_frames: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebMcpAnnotations {
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub read_only: Option<bool>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub untrusted_content: Option<bool>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub consequential: Option<bool>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub debugging: Option<bool>,
  /// A declarative form tool that submits itself instead of waiting for the user.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub autosubmit: Option<bool>,
}

/// The frame a tool was registered in, as it was when the tools were listed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WebMcpFrame {
  pub name: String,
  pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebMcpTool {
  pub name: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub title: Option<String>,
  pub description: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub input_schema: Option<serde_json::Value>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub annotations: Option<WebMcpAnnotations>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub origin: Option<String>,
  /// Registered by a `<form toolname>` rather than by script.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub declarative: bool,
  #[serde(default)]
  pub frame: WebMcpFrame,
}

#[derive(Clone)]
pub struct WebMcp {
  frame: Frame,
}

impl WebMcp {
  pub(crate) fn new(frame: Frame) -> Self {
    Self { frame }
  }

  /// The frame's tools, and its descendants' with `all_frames`.
  ///
  /// # Errors
  /// Returns `Unsupported` when this frame has no `WebMCP`, or a navigation,
  /// evaluation or timeout error when it cannot supply its catalogue.
  pub async fn tools(&self, options: WebMcpOptions) -> Result<Vec<WebMcpTool>> {
    self
      .frame
      .page()
      .traced_as(
        "WebMCP",
        "tools",
        serde_json::json!({}),
        self.within(options, self.catalogue(options.all_frames)),
      )
      .await
  }

  /// Call the tool named `name` and return its result. With `all_frames` the
  /// tool may live in a descendant frame, as long as only one frame has it.
  ///
  /// # Errors
  /// Missing or ambiguous tools, thrown tool exceptions, closed frames and
  /// deadlines reject the call. A result containing `isError: true` remains a
  /// result.
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
        self.within(options, async {
          let (frame, _) = self.resolve(name, options.all_frames).await?;
          if let Some(result) = Box::pin(native_call(&frame, name, &input)).await? {
            return Ok(result);
          }
          Box::pin(page_call(&frame, name, input)).await
        }),
      )
      .await
  }

  /// Wait until a tool named `name` is registered, and return it.
  ///
  /// # Errors
  /// Times out when no such tool appears, naming the reason when the document
  /// has no `WebMCP` at all; a detached frame rejects at once.
  pub async fn wait_for_tool(&self, name: &str, options: WebMcpOptions) -> Result<WebMcpTool> {
    let timeout = options.timeout.unwrap_or_else(|| self.frame.page().default_timeout());
    self
      .frame
      .page()
      .traced_as("WebMCP", "waitForTool", serde_json::json!({"name":name}), async {
        let budget = crate::operation_budget::OperationBudget::operation(timeout)?;
        let mut unavailable = None;
        let poll = async {
          loop {
            match self.catalogue(options.all_frames).await {
              Ok(tools) => {
                unavailable = None;
                if let Some(tool) = tools.into_iter().find(|tool| tool.name == name) {
                  return Ok(tool);
                }
              },
              Err(FerriError::Unsupported(reason)) => unavailable = Some(reason),
              Err(error) if self.frame.is_detached() => return Err(error),
              // A document between navigations has no catalogue yet.
              Err(_) => {},
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
          }
        };
        match budget.scope(budget.wait(poll)).await {
          Ok(found) => found,
          Err(expired) => Err(match unavailable {
            Some(reason) => FerriError::unsupported(reason),
            None => expired.error(format!("waiting for WebMCP tool \"{name}\"")),
          }),
        }
      })
      .await
  }

  async fn within<T>(&self, options: WebMcpOptions, operation: impl Future<Output = Result<T>>) -> Result<T> {
    if self.frame.is_detached() {
      return Err(FerriError::target_closed(Some("WebMCP frame is detached".into())));
    }
    let timeout = options.timeout.unwrap_or_else(|| self.frame.page().default_timeout());
    let budget = crate::operation_budget::OperationBudget::operation(timeout)?;
    budget
      .scope(budget.wait(operation))
      .await
      .map_err(|error| error.error("WebMCP operation"))?
  }

  /// This frame's tools, then each descendant's in document order. A
  /// descendant that has no `WebMCP`, or that goes away while it is asked,
  /// contributes nothing; only this frame's own failure is an error.
  async fn catalogue(&self, all_frames: bool) -> Result<Vec<WebMcpTool>> {
    let mut tools = frame_tools(&self.frame).await?;
    if all_frames {
      let mut pending: Vec<Frame> = self.frame.child_frames().into_iter().rev().collect();
      while let Some(frame) = pending.pop() {
        pending.extend(frame.child_frames().into_iter().rev());
        if let Ok(found) = frame_tools(&frame).await {
          tools.extend(found);
        }
      }
    }
    Ok(tools)
  }

  async fn resolve(&self, name: &str, all_frames: bool) -> Result<(Frame, WebMcpTool)> {
    let mut found: Vec<(Frame, WebMcpTool)> = Vec::new();
    let mut available = Vec::new();
    let mut pending = vec![self.frame.clone()];
    while let Some(frame) = pending.pop() {
      if all_frames {
        pending.extend(frame.child_frames().into_iter().rev());
      }
      let tools = match frame_tools(&frame).await {
        Ok(tools) => tools,
        Err(error) if frame.id() == self.frame.id() => return Err(error),
        Err(_) => continue,
      };
      for tool in tools {
        if tool.name == name {
          found.push((frame.clone(), tool));
        } else {
          available.push(tool.name);
        }
      }
    }
    if found.len() > 1 {
      let frames = found
        .iter()
        .map(|(frame, _)| frame.url())
        .collect::<Vec<_>>()
        .join(", ");
      return Err(FerriError::invalid_argument(
        "name",
        format!("WebMCP tool \"{name}\" is registered by more than one frame ({frames}); call it from that frame"),
      ));
    }
    found.pop().ok_or_else(|| {
      let detail = if available.is_empty() {
        " The frame does not register any WebMCP tools.".to_owned()
      } else {
        format!(" Available tools: {}.", available.join(", "))
      };
      FerriError::evaluation(format!("No WebMCP tool named \"{name}\".{detail}"))
    })
  }
}

async fn frame_tools(frame: &Frame) -> Result<Vec<WebMcpTool>> {
  let source = page_script(None, false, 0)?;
  let listed = evaluate(frame, &source, argument_from_serde(&())?, true).await?;
  let mut tools: Vec<WebMcpTool> = result_to_serde(&operation_result(listed, None)?)?;
  let origin = WebMcpFrame {
    name: frame.name(),
    url: frame.url(),
  };
  for tool in &mut tools {
    tool.frame = origin.clone();
  }
  Ok(tools)
}

/// Call through Chromium's `DevTools` `WebMCP` domain. `None` when this backend
/// or browser has none, so the caller falls back to the page's own API.
async fn native_call(frame: &Frame, name: &str, input: &SerializedArgument) -> Result<Option<SerializedValue>> {
  let page = frame.page();
  if !matches!(page.inner(), AnyPage::CdpPipe(_) | AnyPage::CdpWs(_)) {
    return Ok(None);
  }
  let input = match input.value.to_json_like() {
    None | Some(serde_json::Value::Null) => serde_json::json!({}),
    Some(value @ serde_json::Value::Object(_)) => value,
    Some(_) => {
      return Err(FerriError::invalid_argument(
        "input",
        "a WebMCP tool input must be an object",
      ));
    },
  };
  // Subscribed before the tool is invoked, so a navigation the tool itself
  // causes cannot slip past. Chromium answers neither `toolResponded` nor
  // `toolsRemoved` when the tool's document goes away.
  let mut events = page.events().subscribe();
  let frame_id = frame.frame_id().to_owned();
  let invocation = async {
    match page.inner() {
      AnyPage::CdpPipe(backend) => backend.webmcp_invoke(&frame_id, name, &input).await,
      AnyPage::CdpWs(backend) => backend.webmcp_invoke(&frame_id, name, &input).await,
      _ => Ok(None),
    }
  };
  tokio::pin!(invocation);
  let response = loop {
    tokio::select! {
      response = &mut invocation => break response?,
      event = events.recv() => match event {
        Some(PageEvent::FrameNavigated(info)) if info.frame_id == frame_id => {
          return Err(document_closed(name, "its document navigated away"));
        },
        Some(PageEvent::FrameDetached { frame_id: detached }) if detached == frame_id => {
          return Err(document_closed(name, "its frame was detached"));
        },
        Some(PageEvent::Close) | None => return Err(document_closed(name, "the page closed")),
        Some(_) => {},
      },
    }
  };
  let Some(response) = response else { return Ok(None) };
  match response["status"].as_str() {
    // Chromium hands the result over as JSON text and passes through what
    // does not parse, which for a tool returning `undefined` is the bare
    // word: the same convention the page's own `executeTool()` uses.
    Some("Completed") => Ok(Some(match response.get("output") {
      None => SerializedValue::undefined(),
      Some(serde_json::Value::String(text)) if text == "undefined" => SerializedValue::undefined(),
      Some(output) => argument_from_serde(output)?.value,
    })),
    Some("Canceled") => Err(FerriError::evaluation(format!("WebMCP tool \"{name}\" was canceled"))),
    _ => Err(FerriError::evaluation(format!(
      "WebMCP tool \"{name}\" failed: {}",
      failure_text(&response)
    ))),
  }
}

/// The tool's own error: the thrown exception's message without the stack,
/// else the browser's error text.
fn failure_text(response: &serde_json::Value) -> String {
  let thrown = response["exception"]["description"]
    .as_str()
    .map(|description| description.split("\n    at ").next().unwrap_or(description).trim())
    .filter(|message| !message.is_empty());
  let reported = response["errorText"].as_str().filter(|text| !text.is_empty());
  thrown.or(reported).unwrap_or("the invocation failed").to_owned()
}

fn document_closed(name: &str, cause: &str) -> FerriError {
  FerriError::target_closed(Some(format!("WebMCP tool \"{name}\" was closed: {cause}")))
}

/// Call through the page's `executeTool()`. Firefox only runs tools for
/// callers in the page's own realm, so its call leaves the sandbox.
async fn page_call(frame: &Frame, name: &str, input: SerializedArgument) -> Result<SerializedValue> {
  let firefox = match frame.page().inner() {
    AnyPage::Bidi(page) => page.session.browser_name.eq_ignore_ascii_case("firefox"),
    AnyPage::WebDriver(page) => page.capabilities["browserName"]
      .as_str()
      .is_some_and(|name| name.eq_ignore_ascii_case("firefox")),
    _ => false,
  };
  let deadline = crate::operation_budget::OperationBudget::capture()
    .map(|budget| budget.remaining_ms().map_err(|error| error.error("WebMCP operation")))
    .transpose()?
    .flatten()
    .unwrap_or(0);
  let source = page_script(Some(name), legacy_chromium_input(frame).await?, deadline)?;
  let result = if matches!(frame.page().inner(), AnyPage::WebDriver(_)) {
    Box::pin(classic::call(frame, &source, input)).await?
  } else {
    evaluate(frame, &source, input, !firefox).await?
  };
  operation_result(result, Some((name, deadline)))
}

fn page_script(name: Option<&str>, legacy_input: bool, deadline: u64) -> Result<String> {
  Ok(format!(
    "async input => {{ const toolName = {}; const legacyInput = {legacy_input}; const deadline = {deadline}; {} }}",
    serde_json::to_string(&name)?,
    include_str!("web_mcp.js")
  ))
}

async fn evaluate(frame: &Frame, source: &str, input: SerializedArgument, isolated: bool) -> Result<SerializedValue> {
  if isolated {
    match frame.page().inner() {
      AnyPage::CdpPipe(page) => return page.evaluate_isolated(source, &input, frame.frame_id()).await,
      AnyPage::CdpWs(page) => return page.evaluate_isolated(source, &input, frame.frame_id()).await,
      AnyPage::Bidi(page) => return page.evaluate_isolated(source, &input, frame.frame_id()).await,
      _ => {},
    }
  }
  frame.evaluate_untraced(source, input, Some(true)).await
}

async fn legacy_chromium_input(frame: &Frame) -> Result<bool> {
  let page = frame.page();
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

/// Unwrap the page script's `[true, value]`; `[false]` means the document has
/// no `WebMCP` and `["timeout"]` that the page aborted the call at `deadline`.
fn operation_result(result: SerializedValue, call: Option<(&str, u64)>) -> Result<SerializedValue> {
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
        "WebMCP discovery is unavailable in this document: it needs a secure context and a browser with WebMCP \
         enabled (Chromium `--enable-features=WebMCP`, Firefox `dom.modelcontext.enabled`)",
      ))
    },
    SerializedValue::Array { items, .. }
      if items.len() == 1 && items.first() == Some(&SerializedValue::Str("timeout".into())) =>
    {
      let (name, deadline) = call.ok_or_else(|| FerriError::protocol("WebMCP", "timeout outside a tool call"))?;
      Err(FerriError::timeout(format!("calling WebMCP tool \"{name}\""), deadline))
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
