//! `sidecars` JS binding: connect to a declared sidecar process and drive
//! it with `send(method, params?)` (Promise) plus pushed events. The
//! transport lives in [`crate::sidecar`]; this is the QuickJS surface.
//!
//! Connecting is by declared name only — `sidecars.connect(name)` resolves a
//! `[[sidecars]]` spec the operator configured; scripts cannot spawn an
//! arbitrary process (that would defeat the sandbox). One warm instance per
//! name per session.

use std::sync::Arc;

use rquickjs::class::Trace;
use rquickjs::function::Opt;
use rquickjs::{Class, Ctx, IntoJs, JsLifetime, Value};
use rustc_hash::FxHashMap;
use tokio::sync::Mutex;

use crate::sidecar::{Sidecar, SidecarSpec};

const DEFAULT_SEND_TIMEOUT_MS: u64 = 30_000;

fn throw(ctx: &Ctx<'_>, msg: &str) -> rquickjs::Error {
  rquickjs::Exception::throw_message(ctx, msg)
}

/// The `sidecars` global: the declared specs + the per-session live-connection
/// cache.
#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Sidecars")]
pub struct SidecarsJs {
  #[qjs(skip_trace)]
  specs: Arc<FxHashMap<String, SidecarSpec>>,
  #[qjs(skip_trace)]
  live: Arc<Mutex<FxHashMap<String, Arc<Sidecar>>>>,
}

#[rquickjs::methods]
impl SidecarsJs {
  /// `sidecars.connect(name)` → `Promise<Sidecar>`. Spawns on first connect;
  /// later calls for the same name return the warm instance.
  #[qjs(rename = "connect")]
  pub async fn connect<'js>(&self, ctx: Ctx<'js>, name: String) -> rquickjs::Result<Value<'js>> {
    let Some(spec) = self.specs.get(&name).cloned() else {
      return Err(throw(
        &ctx,
        &format!("sidecars.connect: unknown sidecar '{name}' — declare it under [[sidecars]]"),
      ));
    };
    let inner = {
      let mut live = self.live.lock().await;
      if let Some(existing) = live.get(&name) {
        existing.clone()
      } else {
        let s = Sidecar::connect(&spec).await.map_err(|e| throw(&ctx, &e.to_string()))?;
        live.insert(name, s.clone());
        s
      }
    };
    let wrapper = SidecarJs {
      inner,
      default_timeout_ms: DEFAULT_SEND_TIMEOUT_MS,
    };
    let instance = Class::instance(ctx.clone(), wrapper)?;
    IntoJs::into_js(instance, &ctx)
  }
}

/// A connected sidecar handle.
#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Sidecar")]
pub struct SidecarJs {
  #[qjs(skip_trace)]
  inner: Arc<Sidecar>,
  #[qjs(skip_trace)]
  default_timeout_ms: u64,
}

#[rquickjs::methods]
impl SidecarJs {
  /// `send(method, params?)` → `Promise<result>`. Rejects on a child
  /// `{error}` reply, timeout, or a closed transport.
  #[qjs(rename = "send")]
  pub async fn send<'js>(
    &self,
    ctx: Ctx<'js>,
    method: String,
    params: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let params_json = match params.0 {
      Some(v) if !v.is_null() && !v.is_undefined() => {
        Some(crate::bindings::convert::serde_from_js::<serde_json::Value>(&ctx, v)?)
      },
      _ => None,
    };
    match self.inner.send(&method, params_json, self.default_timeout_ms).await {
      Ok(res) => crate::bindings::convert::serde_to_js(&ctx, &res),
      Err(e) => Err(throw(&ctx, &e.to_string())),
    }
  }

  /// `close()` → `Promise<void>`. Closes the transport and reaps the child.
  #[qjs(rename = "close")]
  pub async fn close(&self, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    self.inner.close().await.map_err(|e| throw(&ctx, &e.to_string()))
  }

  #[qjs(rename = "name")]
  pub fn name(&self) -> String {
    self.inner.name().to_string()
  }
}

/// Install the `sidecars` global. Always installed (even with no declared
/// specs) so `sidecars.connect` exists and rejects unknown names clearly.
pub fn install_sidecars(ctx: &Ctx<'_>, specs: &[SidecarSpec]) -> rquickjs::Result<()> {
  let g = ctx.globals();
  Class::<SidecarsJs>::define(&g)?;
  Class::<SidecarJs>::define(&g)?;
  let mut map = FxHashMap::default();
  for s in specs {
    map.insert(s.name.clone(), s.clone());
  }
  let inst = Class::instance(
    ctx.clone(),
    SidecarsJs {
      specs: Arc::new(map),
      live: Arc::new(Mutex::new(FxHashMap::default())),
    },
  )?;
  g.set("sidecars", inst)?;
  Ok(())
}
