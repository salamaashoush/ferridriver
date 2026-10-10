//! `CredentialsJs`: QuickJS binding for `context.credentials`, the
//! context's virtual WebAuthn authenticator (Playwright `Credentials`).

use ferridriver::credentials::{CredentialCreateOptions, CredentialFilter, Credentials};
use rquickjs::function::Opt;
use rquickjs::{Ctx, JsLifetime, Value, class::Trace};

use crate::bindings::convert::{FerriResultCtxExt, serde_from_js, serde_to_js};

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Credentials")]
pub struct CredentialsJs {
  #[qjs(skip_trace)]
  inner: Credentials,
}

impl CredentialsJs {
  #[must_use]
  pub fn new(inner: Credentials) -> Self {
    Self { inner }
  }
}

fn bag<'js, T: Default + serde::de::DeserializeOwned>(ctx: &Ctx<'js>, value: Opt<Value<'js>>) -> rquickjs::Result<T> {
  match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => serde_from_js(ctx, v),
    _ => Ok(T::default()),
  }
}

#[rquickjs::methods]
impl CredentialsJs {
  /// Playwright: `credentials.create(rpId, options?)`.
  #[qjs(rename = "create")]
  pub async fn create<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    rp_id: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    call_site
      .scope(async move {
        let options: CredentialCreateOptions = bag(&ctx, options)?;
        let credential = self.inner.create(&rp_id, options).await.into_js_with(&ctx)?;
        serde_to_js(&ctx, &credential)
      })
      .await
  }

  /// Playwright: `credentials.get(options?)`.
  #[qjs(rename = "get")]
  pub async fn get<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    call_site
      .scope(async move {
        let filter: CredentialFilter = bag(&ctx, options)?;
        serde_to_js(&ctx, &self.inner.get(filter).await)
      })
      .await
  }

  /// Playwright: `credentials.delete(id)`.
  #[qjs(rename = "delete")]
  pub async fn delete(&self, call_site: crate::bindings::CallSite, id: String) {
    call_site.scope(self.inner.delete(&id)).await;
  }

  /// Playwright: `credentials.install()`.
  #[qjs(rename = "install")]
  pub async fn install(&self, call_site: crate::bindings::CallSite, ctx: Ctx<'_>) -> rquickjs::Result<()> {
    call_site
      .scope(async move { self.inner.install().await.into_js_with(&ctx) })
      .await
  }
}
