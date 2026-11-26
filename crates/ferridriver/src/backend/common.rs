//! Backend-agnostic helpers, constants, and traits shared across CDP, `BiDi`,
//! `WebKit`, and `WebDriver` backends.
//!
//! Centralizing these primitives here removes cross-backend imports (such as
//! `WebKit`/`BiDi`/`WebDriver` reaching into `backend::cdp`) and eliminates duplicate
//! error-splitting and numeric saturation helpers.

use futures::future::BoxFuture;

use crate::error::Result;

/// An owned session or device handle whose lifecycle is tied to a browser
/// instance and must be closed asynchronously when the browser closes.
///
/// Used by `CdpBrowser` (for owned `WebDriver` / Android sessions) and
/// `WebDriverBrowser` (for managed iOS sessions) so backend structs do not
/// import each other or device modules directly.
pub(crate) trait OwnedResource: Send + Sync {
  fn close(&self) -> BoxFuture<'_, Result<()>>;
}

/// Escape a string for safe embedding in JS single-quoted string literals.
/// Handles all characters that could break or inject into JS strings.
#[must_use]
pub fn js_escape(s: &str) -> String {
  let mut out = String::with_capacity(s.len() + 8);
  for c in s.chars() {
    match c {
      '\\' => out.push_str("\\\\"),
      '\'' => out.push_str("\\'"),
      '"' => out.push_str("\\\""),
      '`' => out.push_str("\\`"),
      '\n' => out.push_str("\\n"),
      '\r' => out.push_str("\\r"),
      '\t' => out.push_str("\\t"),
      '\0' => out.push_str("\\0"),
      // Unicode line/paragraph separators (would terminate JS string in some engines)
      '\u{2028}' => out.push_str("\\u2028"),
      '\u{2029}' => out.push_str("\\u2029"),
      _ => out.push(c),
    }
  }
  out
}

/// The JS source for the exposed-binding controller injected as an init script
/// on CDP and `WebKit` pages.
pub const BINDING_CONTROLLER_JS: &str = r"(function(){
if(globalThis.__fd_bc)return;
var bc={seq:0,cbs:{},fns:{}};
globalThis.__fd_bc=bc;
bc.call=function(name,args){
  var s=++bc.seq;
  var p=new Promise(function(r,j){bc.cbs[s]={r:r,j:j}});
  globalThis.__fd_binding__(JSON.stringify({name:name,seq:s,args:args}));
  return p;
};
bc.add=function(name){
  bc.fns[name]=true;
  globalThis[name]=function(){return bc.call(name,Array.prototype.slice.call(arguments))};
};
Object.defineProperty(globalThis,'__playwright__binding__controller__',{configurable:true,value:{
  callBinding:function(name){return bc.call(name,Array.prototype.slice.call(arguments,1))}
}});
bc.del=function(name){delete bc.fns[name];delete globalThis[name]};
bc.resolve=function(seq,val){var c=bc.cbs[seq];if(c){delete bc.cbs[seq];c.r(val)}};
bc.reject=function(seq,err){var c=bc.cbs[seq];if(c){delete bc.cbs[seq];c.j(new Error(err))}};
})()";

/// Sentinel thrown by [`UTILITY_EVAL_WRAPPER`] when a document has not had the
/// injected selector engine installed yet. A backend that injects on demand
/// re-injects and retries when it sees this message.
pub(crate) const ENGINE_MISSING: &str = "ferridriver: selector engine missing";

/// Maximum consecutive navigation-race retries before giving up on injecting
/// the utility script into a document.
pub(crate) const ENGINE_ATTEMPTS: usize = 5;

/// Wrapper function declaration shared by CDP (`Runtime.callFunctionOn`),
/// `BiDi` (`script.callFunction`), `WebKit` (`Runtime.callFunctionOn`), and
/// `WebDriver` (`executeScript`) utility evaluations.
///
/// Memoises the `UtilityScript` instance on `window.__fd.__us`, `JSON.parse`s
/// the serialized-args array, forwards each element as an individual arg into
/// `utilityScript.evaluate`, and `JSON.stringify`s the result back so every
/// backend only has to ship a flat string.
pub(crate) const UTILITY_EVAL_WRAPPER: &str = "function(isFn, retVal, expr, count, serializedArgs, ...handles) {\
    const fd = window.__fd;\
    if (!fd) throw new Error('ferridriver: selector engine missing');\
    const parsed = count > 0 ? JSON.parse(serializedArgs) : [];\
    const us = fd.__us || (fd.__us = fd.newUtilityScript());\
    const result = us.evaluate(isFn, retVal, expr, count, ...parsed, ...handles);\
    if (result && typeof result.then === 'function') {\
      return result.then(r => {\
        if (retVal) {\
          const encoded = JSON.stringify(r);\
          return encoded === undefined ? null : encoded;\
        }\
        return r;\
      });\
    }\
    if (retVal) {\
      const encoded = JSON.stringify(result);\
      return encoded === undefined ? null : encoded;\
    }\
    return result;\
  }";

/// Clamp a JSON-typed `u64` (line/column offsets) down to `u32` without
/// truncating silently.
#[must_use]
pub(crate) fn u64_to_u32_saturating(n: u64) -> u32 {
  u32::try_from(n).unwrap_or(u32::MAX)
}

/// Saturate a fractional JS timestamp or byte length to `u64`. Non-finite and
/// negative values collapse to `0`; values past `u64::MAX` clamp at `u64::MAX`.
#[expect(
  clippy::cast_possible_truncation,
  clippy::cast_sign_loss,
  reason = "float-to-u64 `as` truncates toward zero, saturates at both bounds, and maps NaN to 0"
)]
#[must_use]
pub(crate) fn f64_to_u64_saturating(n: f64) -> u64 {
  if n.is_finite() { n as u64 } else { 0 }
}

/// Split a combined JS error line into `(name, message)` at the first `": "`
/// separator (Playwright's `splitErrorMessage` convention). If absent, `name`
/// is empty and `message` is the full input.
#[must_use]
pub(crate) fn split_error_message(message: &str) -> (String, String) {
  match message.split_once(": ") {
    Some((name, msg)) => (name.to_string(), msg.to_string()),
    None => (String::new(), message.to_string()),
  }
}

/// Build the baseline W3C `WebDriver` `alwaysMatch` capabilities object for
/// `browser_name`, merging any caller-supplied vendor capabilities while
/// preserving `webSocketUrl: true` (and `safari:experimentalWebSocketUrl` for
/// Safari).
#[must_use]
pub(crate) fn webdriver_capabilities(browser_name: &str, extra: Option<&serde_json::Value>) -> serde_json::Value {
  let mut always_match = serde_json::json!({
    "browserName": browser_name,
    "webSocketUrl": true,
    "unhandledPromptBehavior": "ignore"
  });
  if let Some(extra) = extra.and_then(serde_json::Value::as_object)
    && let Some(target) = always_match.as_object_mut()
  {
    target.extend(extra.iter().map(|(key, value)| (key.clone(), value.clone())));
    // Ferridriver drives the returned BiDi socket; a caller cannot disable
    // the capability without making the negotiated session unusable.
    target.insert("webSocketUrl".into(), serde_json::Value::Bool(true));
  }
  if always_match
    .get("browserName")
    .and_then(serde_json::Value::as_str)
    .is_some_and(|name| name.eq_ignore_ascii_case("safari"))
    && let Some(target) = always_match.as_object_mut()
  {
    target
      .entry("safari:experimentalWebSocketUrl")
      .or_insert(serde_json::Value::Bool(true));
  }
  always_match
}
