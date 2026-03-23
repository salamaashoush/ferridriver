//! MCP tool parameter types. Each struct maps to one tool's input schema.

use serde::Deserialize;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NavigateParams {
    #[schemars(description = "Target URL.")]
    pub url: String,
    #[schemars(description = "'load' or 'none'.")]
    pub wait_until: Option<String>,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NewPageParams {
    #[schemars(description = "URL to open.")]
    pub url: Option<String>,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ClosePageParams {
    #[schemars(description = "Page index to close.")]
    pub page_index: usize,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SelectPageParams {
    #[schemars(description = "Page index.")]
    pub page_index: usize,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ClickParams {
    #[schemars(description = "Element ref from snapshot.")]
    pub r#ref: Option<String>,
    #[schemars(description = "CSS selector fallback.")]
    pub selector: Option<String>,
    #[schemars(description = "Double click.")]
    pub double_click: Option<bool>,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ClickAtParams {
    #[schemars(description = "X coordinate.")]
    pub x: f64,
    #[schemars(description = "Y coordinate.")]
    pub y: f64,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct HoverParams {
    #[schemars(description = "Element ref.")]
    pub r#ref: Option<String>,
    #[schemars(description = "CSS selector.")]
    pub selector: Option<String>,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FillParams {
    #[schemars(description = "Element ref.")]
    pub r#ref: Option<String>,
    #[schemars(description = "CSS selector.")]
    pub selector: Option<String>,
    #[schemars(description = "Value to fill.")]
    pub value: String,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TypeTextParams {
    #[schemars(description = "Text to type.")]
    pub text: String,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PressKeyParams {
    #[schemars(description = "Key name or combo.")]
    pub key: String,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DragParams {
    pub from_x: f64,
    pub from_y: f64,
    pub to_x: f64,
    pub to_y: f64,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScrollParams {
    pub delta_x: Option<f64>,
    pub delta_y: Option<f64>,
    #[schemars(description = "Selector to scroll into view.")]
    pub selector: Option<String>,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScreenshotParams_ {
    #[schemars(description = "png, jpeg, or webp.")]
    pub format: Option<String>,
    pub quality: Option<i64>,
    pub full_page: Option<bool>,
    #[schemars(description = "Element selector for partial screenshot.")]
    pub selector: Option<String>,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EvaluateParams {
    #[schemars(description = "JS expression.")]
    pub expression: String,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SnapshotParams {
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WaitForParams {
    pub selector: Option<String>,
    pub text: Option<String>,
    #[schemars(description = "Timeout ms.")]
    pub timeout: Option<u64>,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SessionOnlyParams {
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetCookieParams {
    pub name: String,
    pub value: String,
    pub domain: Option<String>,
    pub path: Option<String>,
    pub secure: Option<bool>,
    pub http_only: Option<bool>,
    pub expires: Option<f64>,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DeleteCookieParams_ {
    pub name: String,
    pub domain: Option<String>,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EmulateDeviceParams {
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub device_scale_factor: Option<f64>,
    pub mobile: Option<bool>,
    pub user_agent: Option<String>,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetGeolocationParams {
    pub latitude: f64,
    pub longitude: f64,
    pub accuracy: Option<f64>,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetNetworkStateParams {
    #[schemars(description = "offline or online.")]
    pub state: String,
    pub download_throughput: Option<f64>,
    pub upload_throughput: Option<f64>,
    pub latency: Option<f64>,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LocalStorageKeyParams {
    pub key: String,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LocalStorageSetParams {
    pub key: String,
    pub value: String,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetContentParams {
    pub html: String,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ConsoleMessagesParams {
    #[schemars(description = "Filter: log, warn, error, info, debug, or all.")]
    pub level: Option<String>,
    #[schemars(description = "Max messages to return.")]
    pub limit: Option<usize>,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NetworkRequestsParams {
    #[schemars(description = "Max requests to return.")]
    pub limit: Option<usize>,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FormField {
    #[schemars(description = "Element ref.")]
    pub r#ref: Option<String>,
    #[schemars(description = "CSS selector.")]
    pub selector: Option<String>,
    #[schemars(description = "Value to fill.")]
    pub value: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FillFormParams {
    #[schemars(description = "Array of {ref, selector, value} fields.")]
    pub fields: Vec<FormField>,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RunScenarioParams {
    #[schemars(description = "Gherkin scenario script (Given/When/Then steps).")]
    pub script: String,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
    #[schemars(description = "Stop on first failure. Default: true.")]
    pub stop_on_failure: Option<bool>,
    #[schemars(description = "Screenshot on failure. Default: false.")]
    pub screenshot_on_failure: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchPageParams {
    #[schemars(description = "Text or regex pattern to search for in page content.")]
    pub pattern: String,
    #[schemars(description = "Treat pattern as regex. Default: false.")]
    pub regex: Option<bool>,
    #[schemars(description = "Case-sensitive search. Default: false.")]
    pub case_sensitive: Option<bool>,
    #[schemars(description = "Characters of surrounding context per match. Default: 150.")]
    pub context_chars: Option<usize>,
    #[schemars(description = "CSS selector to limit search scope.")]
    pub selector: Option<String>,
    #[schemars(description = "Maximum matches to return. Default: 25.")]
    pub max_results: Option<usize>,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FindElementsParams {
    #[schemars(description = "CSS selector to query elements.")]
    pub selector: String,
    #[schemars(description = "Specific attributes to extract (e.g. [\"href\", \"src\"]).")]
    pub attributes: Option<Vec<String>>,
    #[schemars(description = "Maximum elements to return. Default: 50.")]
    pub max_results: Option<usize>,
    #[schemars(description = "Include text content of each element. Default: true.")]
    pub include_text: Option<bool>,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SelectOptionParams {
    #[schemars(description = "Element ref from snapshot.")]
    pub r#ref: Option<String>,
    #[schemars(description = "CSS selector.")]
    pub selector: Option<String>,
    #[schemars(description = "Option value to select.")]
    pub value: Option<String>,
    #[schemars(description = "Option text/label to select.")]
    pub label: Option<String>,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetDropdownOptionsParams {
    #[schemars(description = "Element ref from snapshot.")]
    pub r#ref: Option<String>,
    #[schemars(description = "CSS selector.")]
    pub selector: Option<String>,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UploadFileParams {
    #[schemars(description = "CSS selector for the file input element.")]
    pub selector: String,
    #[schemars(description = "Absolute path to the file to upload.")]
    pub path: String,
    #[schemars(description = "Session name. Defaults to 'default'.")]
    pub session: Option<String>,
}
