//! A WHATWG-ish `fetch` + `Headers` + `Response`, so npm packages that
//! expect `fetch` work. It is a thin surface over the SAME
//! `ferridriver::api_request` core the Playwright-style `request`
//! binding uses — one HTTP stack, one place the net policy applies. The
//! ergonomic `request` API stays; this just adds the standard entry
//! point.
//!
//! Deliberately a subset: `text()` / `json()` / `arrayBuffer()` bodies,
//! request `method` / `headers` / `body` (string or JSON object). No
//! streaming / `Blob` / `FormData` / `AbortController` yet, and the
//! response class is `FetchResponse` (the global `Response` name is the
//! page-network class) so `instanceof Response` does not apply.

use std::sync::Arc;

use ferridriver::api_request::{APIRequestContext, RequestOptions};
use rquickjs::function::Opt;
use rquickjs::{Ctx, IntoJs, Object, Value, class::Class, class::Trace};

use crate::bindings::convert::json_to_js;

#[derive(Trace)]
#[rquickjs::class(rename = "Headers")]
pub struct HeadersJs {
  #[qjs(skip_trace)]
  pairs: Vec<(String, String)>,
}

#[derive(Trace)]
#[rquickjs::class(rename = "FetchResponse")]
pub struct FetchResponseJs {
  #[qjs(skip_trace)]
  status: u16,
  #[qjs(skip_trace)]
  ok: bool,
  #[qjs(skip_trace)]
  status_text: String,
  #[qjs(skip_trace)]
  url: String,
  #[qjs(skip_trace)]
  headers: Vec<(String, String)>,
  #[qjs(skip_trace)]
  body: Vec<u8>,
}

// SAFETY: only owned `'static` data.
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for HeadersJs {
  type Changed<'to> = HeadersJs;
}
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for FetchResponseJs {
  type Changed<'to> = FetchResponseJs;
}

fn header_pairs_from(v: &Value<'_>) -> Vec<(String, String)> {
  if let Ok(h) = Class::<HeadersJs>::from_value(v) {
    return h.borrow().pairs.clone();
  }
  if let Some(obj) = v.as_object() {
    let mut out = Vec::new();
    if let Ok(keys) = obj.keys::<String>().collect::<rquickjs::Result<Vec<_>>>() {
      for k in keys {
        if let Ok(val) = obj.get::<_, String>(k.as_str()) {
          out.push((k, val));
        }
      }
    }
    return out;
  }
  Vec::new()
}

#[rquickjs::methods]
impl HeadersJs {
  #[qjs(constructor)]
  pub fn new(init: Opt<Value<'_>>) -> Self {
    let pairs = init.0.as_ref().map(header_pairs_from).unwrap_or_default();
    Self { pairs }
  }

  #[qjs(rename = "get")]
  pub fn get(&self, name: String) -> Option<String> {
    let n = name.to_ascii_lowercase();
    self
      .pairs
      .iter()
      .find(|(k, _)| k.to_ascii_lowercase() == n)
      .map(|(_, v)| v.clone())
  }

  #[qjs(rename = "has")]
  pub fn has(&self, name: String) -> bool {
    let n = name.to_ascii_lowercase();
    self.pairs.iter().any(|(k, _)| k.to_ascii_lowercase() == n)
  }

  #[qjs(rename = "set")]
  pub fn set(&mut self, name: String, value: String) {
    let n = name.to_ascii_lowercase();
    self.pairs.retain(|(k, _)| k.to_ascii_lowercase() != n);
    self.pairs.push((name, value));
  }

  #[qjs(rename = "append")]
  pub fn append(&mut self, name: String, value: String) {
    self.pairs.push((name, value));
  }

  #[qjs(rename = "delete")]
  pub fn delete(&mut self, name: String) {
    let n = name.to_ascii_lowercase();
    self.pairs.retain(|(k, _)| k.to_ascii_lowercase() != n);
  }

  #[qjs(rename = "entries")]
  pub fn entries(&self) -> Vec<Vec<String>> {
    self.pairs.iter().map(|(k, v)| vec![k.clone(), v.clone()]).collect()
  }

  #[qjs(rename = "keys")]
  pub fn keys(&self) -> Vec<String> {
    self.pairs.iter().map(|(k, _)| k.clone()).collect()
  }
}

#[rquickjs::methods]
impl FetchResponseJs {
  #[qjs(get, rename = "status")]
  pub fn status(&self) -> u16 {
    self.status
  }
  #[qjs(get, rename = "ok")]
  pub fn ok(&self) -> bool {
    self.ok
  }
  #[qjs(get, rename = "statusText")]
  pub fn status_text(&self) -> String {
    self.status_text.clone()
  }
  #[qjs(get, rename = "url")]
  pub fn url(&self) -> String {
    self.url.clone()
  }

  #[qjs(get, rename = "headers")]
  pub fn headers<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Class<'js, HeadersJs>> {
    Class::instance(
      ctx,
      HeadersJs {
        pairs: self.headers.clone(),
      },
    )
  }

  #[qjs(rename = "text")]
  pub fn text(&self) -> String {
    String::from_utf8_lossy(&self.body).into_owned()
  }

  #[qjs(rename = "json")]
  pub fn json<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let v: serde_json::Value = serde_json::from_slice(&self.body)
      .map_err(|e| rquickjs::Error::new_from_js_message("Response.json", "Error", e.to_string()))?;
    json_to_js(&ctx, &v)
  }

  #[qjs(rename = "arrayBuffer")]
  pub fn array_buffer<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    rquickjs::ArrayBuffer::new(ctx.clone(), self.body.clone()).map(rquickjs::ArrayBuffer::into_value)
  }
}

/// Install `globalThis.fetch`, bound to `cx` (the session's HTTP
/// context — same one the `request` binding wraps). Net policy that
/// applies to `request` applies here because it is the same core.
pub fn install(ctx: &Ctx<'_>, cx: Arc<APIRequestContext>) -> rquickjs::Result<()> {
  // Forward into a generic fn so `Ctx`/`Value`/return share one `'js`
  // (an inline closure gives each arg its own lifetime and the returned
  // promise Value cannot be proven to outlive them) — same pattern as
  // the plugin dispatch closure.
  let f = rquickjs::Function::new(ctx.clone(), move |ctx, input, init| {
    do_fetch(ctx, input, init, cx.clone())
  })?;
  ctx.globals().set("fetch", f)?;
  Ok(())
}

fn do_fetch<'js>(
  ctx: Ctx<'js>,
  input: Value<'js>,
  init: Opt<Object<'js>>,
  cx: Arc<APIRequestContext>,
) -> rquickjs::Result<Value<'js>> {
  {
    let url = input
      .as_string()
      .and_then(|s| s.to_string().ok())
      .or_else(|| input.as_object().and_then(|o| o.get::<_, String>("url").ok()))
      .unwrap_or_default();
    let init = init.0;
    let method = init.as_ref().and_then(|o| o.get::<_, String>("method").ok());
    let headers = init
      .as_ref()
      .and_then(|o| o.get::<_, Value<'_>>("headers").ok())
      .map(|v| header_pairs_from(&v));
    // body: string -> raw; object -> JSON (+ content-type unless set).
    let (data, json_data) = match init.as_ref().and_then(|o| o.get::<_, Value<'_>>("body").ok()) {
      Some(b) if b.is_string() => (
        b.as_string().and_then(|s| s.to_string().ok()).map(String::into_bytes),
        None,
      ),
      Some(b) if b.is_object() => {
        let j: Option<serde_json::Value> = crate::bindings::convert::serde_from_js(&ctx, b).ok();
        (None, j)
      },
      _ => (None, None),
    };
    let promised = rquickjs::promise::Promised::from(async move {
      let opts = RequestOptions {
        method,
        headers,
        data,
        json_data,
        ..Default::default()
      };
      let resp = cx
        .fetch(&url, Some(opts))
        .await
        .map_err(|e| rquickjs::Error::new_from_js_message("fetch", "Error", e.to_string()))?;
      let out = FetchResponseJs {
        status: resp.status(),
        ok: resp.ok(),
        status_text: resp.status_text().to_string(),
        url: resp.url().to_string(),
        headers: resp.headers().to_vec(),
        body: resp.text().map(String::into_bytes).unwrap_or_default(),
      };
      Ok::<_, rquickjs::Error>(out)
    });
    promised.into_js(&ctx)
  }
}
