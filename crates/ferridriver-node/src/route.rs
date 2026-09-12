//! NAPI Route class — mirrors Playwright's Route interface.
//!
//! The handler receives a Route object and must call exactly one of
//! `fulfill()`, `continue_()`, or `abort()` to resume the paused request.

use napi::bindgen_prelude::{Buffer, ClassInstance, Either, JsObjectValue, Object, PromiseRaw};
use napi::{Env, Result, ValueType};
use napi_derive::napi;
use std::sync::{Arc, Mutex};

/// A paused network request. Call `fulfill()`, `continue_()`, or `abort()` to resume.
#[napi]
pub struct Route {
  inner: Arc<Mutex<Option<ferridriver::route::Route>>>,
}

impl Route {
  fn lock(&self) -> Result<std::sync::MutexGuard<'_, Option<ferridriver::route::Route>>> {
    self
      .inner
      .lock()
      .map_err(|error| napi::Error::from_reason(error.to_string()))
  }

  pub(crate) fn wrap(inner: ferridriver::route::Route) -> Self {
    Self {
      inner: Arc::new(Mutex::new(Some(inner))),
    }
  }
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
  pub fn url(&self) -> Result<String> {
    Ok(
      self
        .lock()?
        .as_ref()
        .map(|r| r.request().url.clone())
        .unwrap_or_default(),
    )
  }

  /// The HTTP method of the intercepted request.
  #[napi(getter)]
  pub fn method(&self) -> Result<String> {
    Ok(
      self
        .lock()?
        .as_ref()
        .map(|r| r.request().method.clone())
        .unwrap_or_default(),
    )
  }

  /// The resource type (Document, Script, Stylesheet, Image, etc.).
  #[napi(getter)]
  pub fn resource_type(&self) -> Result<String> {
    Ok(
      self
        .lock()?
        .as_ref()
        .map(|r| r.request().resource_type.clone())
        .unwrap_or_default(),
    )
  }

  /// The POST body of the intercepted request, if any.
  #[napi(getter)]
  pub fn post_data(&self) -> Result<Option<String>> {
    Ok(self.lock()?.as_ref().and_then(|r| r.request().post_data.clone()))
  }

  /// Mirrors Playwright `route.request(): Request` — the full Request
  /// API view over the intercepted request.
  #[napi]
  pub fn request(&self) -> Result<crate::network::Request> {
    let guard = self.lock()?;
    let inner = guard
      .as_ref()
      .ok_or_else(|| napi::Error::from_reason("Route already handled"))?;
    Ok(crate::network::Request::from_core(inner.network_request()))
  }

  /// The request headers as a JSON object.
  #[napi(getter)]
  pub fn headers(&self) -> Result<serde_json::Value> {
    Ok(
      self
        .lock()?
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
        .unwrap_or(serde_json::Value::Object(Default::default())),
    )
  }

  #[napi(
    ts_args_type = "options?: { response?: HttpResponse; status?: number; headers?: Record<string, string>; contentType?: string; body?: string | Buffer; json?: any; path?: string }",
    ts_return_type = "Promise<void>"
  )]
  pub fn fulfill<'env>(&mut self, env: &'env Env, options: Option<Object<'env>>) -> Result<PromiseRaw<'env, ()>> {
    let mut opts = ferridriver::route::FulfillOptions::default();
    if let Some(bag) = options {
      opts.status = bag.get("status")?;
      opts.content_type = bag.get("contentType")?;
      opts.path = bag.get::<String>("path")?.map(Into::into);
      opts.headers = bag
        .get::<std::collections::HashMap<String, String>>("headers")?
        .map(|headers| headers.into_iter().collect());
      opts.body = bag.get::<Either<String, Buffer>>("body")?.map(|body| match body {
        Either::A(text) => text.into_bytes(),
        Either::B(bytes) => bytes.to_vec(),
      });
      let json = bag.get_named_property_unchecked::<napi::Unknown<'_>>("json")?;
      if json.get_type()? != ValueType::Undefined {
        opts.json = Some(env.from_js_value(json)?);
      }
      opts.response = bag
        .get::<ClassInstance<'_, crate::http_client::HttpResponse>>("response")?
        .map(|response| response.inner.clone());
    }
    let shared = self.inner.clone();
    env.spawn_future(async move {
      let response = opts
        .resolve()
        .await
        .map_err(|error| napi::Error::from_reason(error.to_string()))?;
      let inner = shared
        .lock()
        .map_err(|error| napi::Error::from_reason(error.to_string()))?
        .take()
        .ok_or_else(|| napi::Error::from_reason("Route already handled"))?;
      inner.fulfill(response);
      Ok(())
    })
  }

  /// Continue the request, optionally with modifications.
  #[napi(js_name = "continue")]
  pub fn continue_route(&mut self, options: Option<ContinueOptions>) -> Result<()> {
    let inner = self
      .lock()?
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

  /// Mirrors Playwright `route.fallback(options?)`
  /// (`client/network.ts`): hand the request to the next matching
  /// handler, applying the given overrides. ferridriver dispatches one
  /// handler per matched route, so `fallback` resolves the route by
  /// continuing the request with the overrides applied (with no
  /// overrides this is the unmodified request, which is the end state
  /// Playwright's `fallback` reaches once no further handler claims it).
  #[napi]
  pub fn fallback(&mut self, options: Option<ContinueOptions>) -> Result<()> {
    let inner = self
      .lock()?
      .take()
      .ok_or_else(|| napi::Error::from_reason("Route already handled"))?;

    let opts = options.unwrap_or_default();
    inner.fallback(ferridriver::route::ContinueOverrides {
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
      .lock()?
      .take()
      .ok_or_else(|| napi::Error::from_reason("Route already handled"))?;

    inner.abort(&reason.unwrap_or_else(|| "blockedbyclient".into()));
    Ok(())
  }
}
