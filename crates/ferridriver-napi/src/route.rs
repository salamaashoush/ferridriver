//! NAPI Route class — mirrors Playwright's Route interface.
//!
//! The handler receives a Route object and must call exactly one of
//! `fulfill()`, `continue_()`, or `abort()` to resume the paused request.

use napi::Result;
use napi_derive::napi;

/// A paused network request. Call `fulfill()`, `continue_()`, or `abort()` to resume.
#[napi]
pub struct Route {
  inner: Option<ferridriver::route::Route>,
}

impl Route {
  pub(crate) fn wrap(inner: ferridriver::route::Route) -> Self {
    Self { inner: Some(inner) }
  }
}

/// Options for `route.fulfill()`.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct FulfillOptions {
  /// HTTP status code (default: 200).
  pub status: Option<i32>,
  /// Response body as string.
  pub body: Option<String>,
  /// Content-Type header.
  pub content_type: Option<String>,
  /// Response headers as `[[key, value], ...]`.
  pub headers: Option<Vec<Vec<String>>>,
}

/// Options for `route.continue_()`.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct ContinueOptions {
  /// Override request URL.
  pub url: Option<String>,
  /// Override HTTP method.
  pub method: Option<String>,
  /// Override request headers as `[[key, value], ...]`.
  pub headers: Option<Vec<Vec<String>>>,
  /// Override POST body.
  pub post_data: Option<String>,
}

#[napi]
impl Route {
  /// The URL of the intercepted request.
  #[napi(getter)]
  pub fn url(&self) -> String {
    self
      .inner
      .as_ref()
      .map(|r| r.request().url.clone())
      .unwrap_or_default()
  }

  /// The HTTP method of the intercepted request.
  #[napi(getter)]
  pub fn method(&self) -> String {
    self
      .inner
      .as_ref()
      .map(|r| r.request().method.clone())
      .unwrap_or_default()
  }

  /// The resource type (Document, Script, Stylesheet, Image, etc.).
  #[napi(getter)]
  pub fn resource_type(&self) -> String {
    self
      .inner
      .as_ref()
      .map(|r| r.request().resource_type.clone())
      .unwrap_or_default()
  }

  /// The POST body of the intercepted request, if any.
  #[napi(getter)]
  pub fn post_data(&self) -> Option<String> {
    self.inner.as_ref().and_then(|r| r.request().post_data.clone())
  }

  /// The request headers as a JSON object.
  #[napi(getter)]
  pub fn headers(&self) -> serde_json::Value {
    self
      .inner
      .as_ref()
      .map(|r| {
        let map: serde_json::Map<String, serde_json::Value> = r
          .request()
          .headers
          .iter()
          .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
          .collect();
        serde_json::Value::Object(map)
      })
      .unwrap_or(serde_json::Value::Object(Default::default()))
  }

  /// Fulfill the request with a custom response (mock).
  #[napi]
  pub fn fulfill(&mut self, options: Option<FulfillOptions>) -> Result<()> {
    let inner = self
      .inner
      .take()
      .ok_or_else(|| napi::Error::from_reason("Route already handled"))?;

    let opts = options.unwrap_or_default();
    inner.fulfill(ferridriver::route::FulfillResponse {
      status: opts.status.unwrap_or(200),
      headers: opts
        .headers
        .as_ref()
        .map(|h| {
          h.iter()
            .filter_map(|pair| {
              if pair.len() == 2 {
                Some((pair[0].clone(), pair[1].clone()))
              } else {
                None
              }
            })
            .collect()
        })
        .unwrap_or_default(),
      body: opts.body.unwrap_or_default().into_bytes(),
      content_type: opts.content_type,
    });
    Ok(())
  }

  /// Continue the request, optionally with modifications.
  #[napi(js_name = "continue")]
  pub fn continue_route(&mut self, options: Option<ContinueOptions>) -> Result<()> {
    let inner = self
      .inner
      .take()
      .ok_or_else(|| napi::Error::from_reason("Route already handled"))?;

    let opts = options.unwrap_or_default();
    inner.continue_route(ferridriver::route::ContinueOverrides {
      url: opts.url,
      method: opts.method,
      headers: opts.headers.as_ref().map(|h| {
        h.iter()
          .filter_map(|pair| {
            if pair.len() == 2 {
              Some((pair[0].clone(), pair[1].clone()))
            } else {
              None
            }
          })
          .collect()
      }),
      post_data: opts.post_data.map(String::into_bytes),
    });
    Ok(())
  }

  /// Abort the request.
  #[napi]
  pub fn abort(&mut self, reason: Option<String>) -> Result<()> {
    let inner = self
      .inner
      .take()
      .ok_or_else(|| napi::Error::from_reason("Route already handled"))?;

    inner.abort(&reason.unwrap_or_else(|| "blockedbyclient".into()));
    Ok(())
  }
}
