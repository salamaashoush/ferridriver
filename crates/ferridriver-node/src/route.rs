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

/// Options for `route.continue(options?)` and `route.fallback(options?)`.
#[napi(object)]
pub struct ContinueOptions {
  /// Override request URL.
  pub url: Option<String>,
  /// Override HTTP method.
  pub method: Option<String>,
  /// Override request headers.
  pub headers: Option<std::collections::HashMap<String, String>>,
  /// Override POST body.
  pub post_data: Option<Either<String, Buffer>>,
}

impl ContinueOptions {
  fn into_core(self) -> ferridriver::route::ContinueOverrides {
    ferridriver::route::ContinueOverrides {
      url: self.url,
      method: self.method,
      headers: self.headers.map(|headers| headers.into_iter().collect()),
      post_data: self.post_data.map(body_bytes),
    }
  }
}

fn body_bytes(body: Either<String, Buffer>) -> Vec<u8> {
  match body {
    Either::A(text) => text.into_bytes(),
    Either::B(bytes) => bytes.to_vec(),
  }
}

/// Options for `route.fetch(options?)`.
#[napi(object)]
pub struct RouteFetchOptions {
  /// Override request URL.
  pub url: Option<String>,
  /// Override HTTP method.
  pub method: Option<String>,
  /// Override request headers.
  pub headers: Option<std::collections::HashMap<String, String>>,
  /// Override POST body.
  pub post_data: Option<Either<String, Buffer>>,
  /// Redirects to follow; 0 follows none. 20 when unset.
  pub max_redirects: Option<u32>,
  /// Retries on a reset connection.
  pub max_retries: Option<u32>,
  /// Milliseconds for the whole request; 0 waits forever.
  pub timeout: Option<f64>,
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
    ts_args_type = "options?: { response?: APIResponse; status?: number; headers?: Record<string, string>; contentType?: string; body?: string | Buffer; json?: any; path?: string }",
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

  /// Playwright: `route.fetch(options?): Promise<APIResponse>`.
  #[napi(ts_return_type = "Promise<APIResponse>")]
  pub async fn fetch(&self, options: Option<RouteFetchOptions>) -> Result<crate::http_client::HttpResponse> {
    let mut fetch = ferridriver::route::RouteFetchOptions::default();
    if let Some(options) = options {
      fetch.url = options.url;
      fetch.method = options.method;
      fetch.headers = options.headers.map(|headers| headers.into_iter().collect());
      fetch.post_data = options.post_data.map(body_bytes);
      fetch.max_redirects = options.max_redirects;
      fetch.max_retries = options.max_retries;
      fetch.timeout = options.timeout.map(crate::types::f64_to_u64);
    }
    let (request, client) = {
      let route = self.lock()?;
      let route = route
        .as_ref()
        .ok_or_else(|| napi::Error::from_reason("Route already handled"))?;
      (
        route.request().clone(),
        route.fetch_client().map_err(crate::error::to_napi)?,
      )
    };
    let inner = request
      .fetch(&client, fetch, None)
      .await
      .map_err(crate::error::to_napi)?;
    Ok(crate::http_client::HttpResponse { inner })
  }

  /// Continue the request, optionally with modifications.
  #[napi(js_name = "continue")]
  pub fn continue_route(&mut self, options: Option<ContinueOptions>) -> Result<()> {
    let inner = self
      .lock()?
      .take()
      .ok_or_else(|| napi::Error::from_reason("Route already handled"))?;

    inner.continue_route(options.map(ContinueOptions::into_core).unwrap_or_default());
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

    inner.fallback(options.map(ContinueOptions::into_core).unwrap_or_default());
    Ok(())
  }

  /// Playwright: `route.abort(errorCode?)`.
  #[napi]
  pub fn abort(&mut self, error_code: Option<String>) -> Result<()> {
    let inner = self
      .lock()?
      .take()
      .ok_or_else(|| napi::Error::from_reason("Route already handled"))?;

    inner.abort(error_code.as_deref());
    Ok(())
  }
}
