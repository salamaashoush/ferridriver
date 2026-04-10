//! WebDriver BiDi protocol types.
//!
//! Covers the full BiDi type system: remote/local values, element references,
//! locators, evaluate results, screenshot options, and input action types.

use serde::{Deserialize, Serialize};

// ── Element References ─────────────────────────────────────────────────────

/// BiDi element reference -- the fundamental element handle.
/// Unlike CDP's numeric nodeId, BiDi uses string-based SharedReferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedReference {
  #[serde(rename = "sharedId")]
  pub shared_id: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub handle: Option<String>,
}

// ── Script Target ──────────────────────────────────────────────────────────

/// Script execution target -- either a browsing context or a realm.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ScriptTarget {
  Context { context: String },
  Realm { realm: String },
}

// ── Remote Values (received from browser) ──────────────────────────────────

/// Serialized JavaScript value returned by `script.evaluate` / `script.callFunction`.
/// Covers the full BiDi RemoteValue type hierarchy.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum RemoteValue {
  #[serde(rename = "undefined")]
  Undefined,
  #[serde(rename = "null")]
  Null,
  #[serde(rename = "string")]
  String { value: serde_json::Value },
  #[serde(rename = "number")]
  Number { value: serde_json::Value },
  #[serde(rename = "boolean")]
  Boolean { value: bool },
  #[serde(rename = "bigint")]
  BigInt { value: serde_json::Value },
  #[serde(rename = "array")]
  Array {
    value: Option<Vec<RemoteValue>>,
    handle: Option<String>,
    #[serde(rename = "internalId")]
    internal_id: Option<String>,
  },
  #[serde(rename = "object")]
  Object {
    /// Object entries: each entry is a 2-element array [key, value].
    /// Key can be a bare string OR a RemoteValue, value is always a RemoteValue.
    /// We use raw JSON to handle both formats.
    value: Option<Vec<serde_json::Value>>,
    handle: Option<String>,
    #[serde(rename = "internalId")]
    internal_id: Option<String>,
  },
  #[serde(rename = "node")]
  Node {
    #[serde(rename = "sharedId")]
    shared_id: Option<String>,
    handle: Option<String>,
    value: Option<Box<NodeProperties>>,
  },
  #[serde(rename = "window")]
  Window {
    value: WindowProxyProperties,
    handle: Option<String>,
  },
  #[serde(rename = "regexp")]
  RegExp { value: RegExpValue },
  #[serde(rename = "date")]
  Date { value: serde_json::Value },
  #[serde(rename = "map")]
  Map {
    value: Option<Vec<serde_json::Value>>,
    handle: Option<String>,
  },
  #[serde(rename = "set")]
  Set {
    value: Option<Vec<RemoteValue>>,
    handle: Option<String>,
  },
  #[serde(rename = "symbol")]
  Symbol { handle: Option<String> },
  #[serde(rename = "function")]
  Function { handle: Option<String> },
  #[serde(rename = "error")]
  Error { handle: Option<String> },
  #[serde(rename = "promise")]
  Promise { handle: Option<String> },
  #[serde(rename = "typedarray")]
  TypedArray { handle: Option<String> },
  #[serde(rename = "arraybuffer")]
  ArrayBuffer { handle: Option<String> },
  #[serde(rename = "nodelist")]
  NodeList {
    value: Option<Vec<RemoteValue>>,
    handle: Option<String>,
  },
  #[serde(rename = "htmlcollection")]
  HtmlCollection {
    value: Option<Vec<RemoteValue>>,
    handle: Option<String>,
  },
  #[serde(rename = "weakmap")]
  WeakMap { handle: Option<String> },
  #[serde(rename = "weakset")]
  WeakSet { handle: Option<String> },
  #[serde(rename = "generator")]
  Generator { handle: Option<String> },
  #[serde(rename = "proxy")]
  Proxy { handle: Option<String> },
  #[serde(rename = "iterator")]
  Iterator { handle: Option<String> },
  #[serde(rename = "weakref")]
  WeakRef { handle: Option<String> },
}

#[derive(Debug, Clone, Deserialize)]
pub struct WindowProxyProperties {
  pub context: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegExpValue {
  pub pattern: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub flags: Option<String>,
}

/// Node properties when a DOM node is serialized.
#[derive(Debug, Clone, Deserialize)]
pub struct NodeProperties {
  #[serde(rename = "nodeType")]
  pub node_type: u32,
  #[serde(rename = "childNodeCount")]
  pub child_node_count: u32,
  #[serde(rename = "localName")]
  pub local_name: Option<String>,
  pub attributes: Option<serde_json::Map<String, serde_json::Value>>,
  #[serde(rename = "nodeValue")]
  pub node_value: Option<String>,
  pub children: Option<Vec<RemoteValue>>,
  #[serde(rename = "shadowRoot")]
  pub shadow_root: Option<Box<RemoteValue>>,
  #[serde(rename = "namespaceURI")]
  pub namespace_uri: Option<String>,
}

impl RemoteValue {
  /// Convert a BiDi `RemoteValue` to a `serde_json::Value` for API compatibility.
  /// This matches the CDP backend's evaluate return type.
  pub fn to_json(&self) -> Option<serde_json::Value> {
    match self {
      Self::Undefined | Self::Null => None,
      Self::String { value } => {
        // value can be a JSON string like "hello" or the raw string
        if let Some(s) = value.as_str() {
          Some(serde_json::Value::String(s.to_string()))
        } else {
          Some(value.clone())
        }
      },
      Self::Number { value } => {
        // BiDi can return special values like "NaN", "Infinity", "-Infinity", "-0"
        if let Some(s) = value.as_str() {
          match s {
            "NaN" => Some(serde_json::json!(null)),
            "Infinity" => Some(serde_json::json!(f64::INFINITY)),
            "-Infinity" => Some(serde_json::json!(f64::NEG_INFINITY)),
            "-0" => Some(serde_json::json!(-0.0_f64)),
            _ => Some(value.clone()),
          }
        } else {
          Some(value.clone())
        }
      },
      Self::Boolean { value } => Some(serde_json::Value::Bool(*value)),
      Self::BigInt { value } => {
        // Return bigint as string representation
        Some(value.clone())
      },
      Self::Array { value, .. } => {
        let arr = value
          .as_ref()
          .map(|items| {
            items
              .iter()
              .map(|v| v.to_json().unwrap_or(serde_json::Value::Null))
              .collect::<Vec<_>>()
          })
          .unwrap_or_default();
        Some(serde_json::Value::Array(arr))
      },
      Self::Object { value, .. } => {
        let mut map = serde_json::Map::new();
        if let Some(entries) = value {
          for entry in entries {
            if let Some(arr) = entry.as_array() {
              if arr.len() == 2 {
                // Key can be a bare string or a RemoteValue
                let key = if let Some(s) = arr[0].as_str() {
                  s.to_string()
                } else if let Ok(rv) = serde_json::from_value::<RemoteValue>(arr[0].clone()) {
                  rv.to_json()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default()
                } else {
                  continue;
                };
                // Value is always a RemoteValue
                let val = if let Ok(rv) = serde_json::from_value::<RemoteValue>(arr[1].clone()) {
                  rv.to_json().unwrap_or(serde_json::Value::Null)
                } else {
                  arr[1].clone()
                };
                map.insert(key, val);
              }
            }
          }
        }
        Some(serde_json::Value::Object(map))
      },
      Self::Map { value, .. } => {
        // Convert Map to JSON object (same format as Object entries)
        let mut map = serde_json::Map::new();
        if let Some(entries) = value {
          for entry in entries {
            if let Some(arr) = entry.as_array() {
              if arr.len() == 2 {
                let key = if let Some(s) = arr[0].as_str() {
                  s.to_string()
                } else if let Ok(rv) = serde_json::from_value::<RemoteValue>(arr[0].clone()) {
                  rv.to_json()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default()
                } else {
                  continue;
                };
                let val = if let Ok(rv) = serde_json::from_value::<RemoteValue>(arr[1].clone()) {
                  rv.to_json().unwrap_or(serde_json::Value::Null)
                } else {
                  arr[1].clone()
                };
                map.insert(key, val);
              }
            }
          }
        }
        Some(serde_json::Value::Object(map))
      },
      Self::Set { value, .. } | Self::NodeList { value, .. } | Self::HtmlCollection { value, .. } => {
        let arr = value
          .as_ref()
          .map(|items| {
            items
              .iter()
              .map(|v| v.to_json().unwrap_or(serde_json::Value::Null))
              .collect::<Vec<_>>()
          })
          .unwrap_or_default();
        Some(serde_json::Value::Array(arr))
      },
      Self::RegExp { value } => {
        let flags = value.flags.as_deref().unwrap_or("");
        Some(serde_json::Value::String(format!("/{}/{flags}", value.pattern)))
      },
      Self::Date { value } => Some(value.clone()),
      Self::Node { value, shared_id, .. } => {
        // Return a simplified node representation
        let mut map = serde_json::Map::new();
        if let Some(sid) = shared_id {
          map.insert("sharedId".to_string(), serde_json::Value::String(sid.clone()));
        }
        if let Some(props) = value {
          map.insert("nodeType".to_string(), serde_json::json!(props.node_type));
          if let Some(ref name) = props.local_name {
            map.insert("localName".to_string(), serde_json::Value::String(name.clone()));
          }
          if let Some(ref nv) = props.node_value {
            map.insert("nodeValue".to_string(), serde_json::Value::String(nv.clone()));
          }
        }
        Some(serde_json::Value::Object(map))
      },
      Self::Window { value, .. } => Some(serde_json::json!({"context": value.context})),
      // Handle types we can't really serialize -- return null or a type marker
      Self::Function { .. }
      | Self::Symbol { .. }
      | Self::Error { .. }
      | Self::Promise { .. }
      | Self::TypedArray { .. }
      | Self::ArrayBuffer { .. }
      | Self::WeakMap { .. }
      | Self::WeakSet { .. }
      | Self::Generator { .. }
      | Self::Proxy { .. }
      | Self::Iterator { .. }
      | Self::WeakRef { .. } => None,
    }
  }

  /// Extract a SharedReference from a Node result.
  pub fn as_shared_reference(&self) -> Option<SharedReference> {
    match self {
      Self::Node { shared_id, handle, .. } => shared_id.as_ref().map(|sid| SharedReference {
        shared_id: sid.clone(),
        handle: handle.clone(),
      }),
      _ => None,
    }
  }
}

// ── Local Values (sent to browser) ─────────────────────────────────────────

/// Value sent as argument to `script.callFunction` / `script.evaluate`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum LocalValue {
  #[serde(rename = "undefined")]
  Undefined,
  #[serde(rename = "null")]
  Null,
  #[serde(rename = "string")]
  String { value: String },
  #[serde(rename = "number")]
  Number { value: serde_json::Value },
  #[serde(rename = "boolean")]
  Boolean { value: bool },
  #[serde(rename = "channel")]
  Channel { value: ChannelValue },
  #[serde(rename = "array")]
  Array { value: Vec<LocalValue> },
  #[serde(rename = "object")]
  Object { value: Vec<Vec<LocalValue>> },
}

/// Channel reference for BiDi message passing.
#[derive(Debug, Clone, Serialize)]
pub struct ChannelValue {
  #[serde(rename = "type")]
  pub channel_type: String,
  pub value: ChannelProperties,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelProperties {
  pub channel: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub ownership: Option<String>,
}

impl LocalValue {
  /// Convert a `serde_json::Value` to a BiDi `LocalValue`.
  pub fn from_json(v: &serde_json::Value) -> Self {
    match v {
      serde_json::Value::Null => Self::Null,
      serde_json::Value::Bool(b) => Self::Boolean { value: *b },
      serde_json::Value::Number(n) => Self::Number {
        value: serde_json::Value::Number(n.clone()),
      },
      serde_json::Value::String(s) => Self::String { value: s.clone() },
      serde_json::Value::Array(arr) => Self::Array {
        value: arr.iter().map(Self::from_json).collect(),
      },
      serde_json::Value::Object(map) => Self::Object {
        value: map
          .iter()
          .map(|(k, v)| vec![Self::String { value: k.clone() }, Self::from_json(v)])
          .collect(),
      },
    }
  }
}

// ── Evaluate Result ────────────────────────────────────────────────────────

/// Result of `script.evaluate` or `script.callFunction`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum EvaluateResult {
  #[serde(rename = "success")]
  Success { result: RemoteValue },
  #[serde(rename = "exception")]
  Exception {
    #[serde(rename = "exceptionDetails")]
    exception_details: ExceptionDetails,
  },
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExceptionDetails {
  #[serde(rename = "columnNumber")]
  pub column_number: Option<u32>,
  pub exception: Option<RemoteValue>,
  #[serde(rename = "lineNumber")]
  pub line_number: Option<u32>,
  #[serde(rename = "stackTrace")]
  pub stack_trace: Option<StackTrace>,
  pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StackTrace {
  #[serde(rename = "callFrames")]
  pub call_frames: Vec<StackFrame>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StackFrame {
  #[serde(rename = "columnNumber")]
  pub column_number: u32,
  #[serde(rename = "functionName")]
  pub function_name: String,
  #[serde(rename = "lineNumber")]
  pub line_number: u32,
  pub url: String,
}

// ── Locators ───────────────────────────────────────────────────────────────

/// Locator for `browsingContext.locateNodes`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum Locator {
  #[serde(rename = "css")]
  Css { value: String },
  #[serde(rename = "xpath")]
  XPath { value: String },
  #[serde(rename = "innerText")]
  InnerText {
    value: String,
    #[serde(rename = "maxDepth", skip_serializing_if = "Option::is_none")]
    max_depth: Option<u32>,
  },
  #[serde(rename = "accessibility")]
  Accessibility { value: AccessibilityLocatorValue },
}

#[derive(Debug, Clone, Serialize)]
pub struct AccessibilityLocatorValue {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub name: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub role: Option<String>,
}

// ── Screenshot Types ───────────────────────────────────────────────────────

/// Clip rectangle for `browsingContext.captureScreenshot`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ClipRectangle {
  #[serde(rename = "box")]
  Box { x: f64, y: f64, width: f64, height: f64 },
  #[serde(rename = "element")]
  Element { element: SharedReference },
}

/// Image format for BiDi screenshots.
#[derive(Debug, Clone, Serialize)]
pub struct BidiImageFormat {
  #[serde(rename = "type")]
  pub format_type: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub quality: Option<f64>,
}

// ── Cookie Types ───────────────────────────────────────────────────────────

/// Partition descriptor for storage commands.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum PartitionDescriptor {
  #[serde(rename = "context")]
  Context { context: String },
  #[serde(rename = "storageKey")]
  StorageKey {
    #[serde(rename = "userContext", skip_serializing_if = "Option::is_none")]
    user_context: Option<String>,
    #[serde(rename = "sourceOrigin", skip_serializing_if = "Option::is_none")]
    source_origin: Option<String>,
  },
}

/// Network bytes value (used in cookies, headers, request/response bodies).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BytesValue {
  #[serde(rename = "string")]
  String { value: String },
  #[serde(rename = "base64")]
  Base64 { value: String },
}

// ── Network Interception Types ─────────────────────────────────────────────

/// URL pattern for network interception.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum UrlPattern {
  #[serde(rename = "pattern")]
  Pattern {
    #[serde(skip_serializing_if = "Option::is_none")]
    protocol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hostname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pathname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    search: Option<String>,
  },
  #[serde(rename = "string")]
  String { pattern: String },
}

/// Network header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkHeader {
  pub name: String,
  pub value: BytesValue,
}

// ── Browsing Context Info ──────────────────────────────────────────────────

/// Context info from `browsingContext.getTree`.
#[derive(Debug, Clone, Deserialize)]
pub struct ContextInfo {
  pub context: String,
  pub url: String,
  #[serde(default)]
  pub children: Vec<ContextInfo>,
  #[serde(rename = "originalOpener")]
  pub original_opener: Option<String>,
  #[serde(rename = "userContext")]
  pub user_context: Option<String>,
  pub parent: Option<String>,
}

/// Navigation info from navigate/reload commands.
#[derive(Debug, Clone, Deserialize)]
pub struct NavigationInfo {
  pub navigation: Option<String>,
  pub url: String,
}
