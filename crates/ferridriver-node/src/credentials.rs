//! `Credentials` — NAPI binding for `browserContext.credentials`, the
//! context's virtual WebAuthn authenticator (Playwright `Credentials`,
//! `types.d.ts:21202`).

use napi::Result;
use napi_derive::napi;

use crate::error::IntoNapi;

/// A passkey: every binary field base64url.
#[napi(object)]
#[derive(Clone)]
pub struct VirtualCredential {
  pub id: String,
  pub rp_id: String,
  pub user_handle: String,
  pub private_key: String,
  pub public_key: String,
  pub sign_count: u32,
}

impl From<ferridriver::credentials::VirtualCredential> for VirtualCredential {
  fn from(c: ferridriver::credentials::VirtualCredential) -> Self {
    Self {
      id: c.id,
      rp_id: c.rp_id,
      user_handle: c.user_handle,
      private_key: c.private_key,
      public_key: c.public_key,
      sign_count: c.sign_count,
    }
  }
}

impl From<VirtualCredential> for ferridriver::credentials::VirtualCredential {
  fn from(c: VirtualCredential) -> Self {
    Self {
      id: c.id,
      rp_id: c.rp_id,
      user_handle: c.user_handle,
      private_key: c.private_key,
      public_key: c.public_key,
      sign_count: c.sign_count,
    }
  }
}

/// `credentials.create(rpId, options?)`.
#[napi(object)]
pub struct CredentialCreateOptions {
  pub id: Option<String>,
  pub user_handle: Option<String>,
  pub private_key: Option<String>,
  pub public_key: Option<String>,
  pub sign_count: Option<i64>,
}

/// `credentials.get(options?)`.
#[napi(object)]
pub struct CredentialFilter {
  pub id: Option<String>,
  pub rp_id: Option<String>,
}

#[napi]
pub struct Credentials {
  inner: ferridriver::credentials::Credentials,
}

impl Credentials {
  pub(crate) fn wrap(inner: ferridriver::credentials::Credentials) -> Self {
    Self { inner }
  }
}

#[napi]
impl Credentials {
  /// Seed a discoverable credential for `rpId`; whatever `options` leaves
  /// out is generated.
  #[napi]
  pub async fn create(&self, rp_id: String, options: Option<CredentialCreateOptions>) -> Result<VirtualCredential> {
    let options = options.map_or_else(Default::default, |o| {
      ferridriver::credentials::CredentialCreateOptions {
        id: o.id,
        user_handle: o.user_handle,
        private_key: o.private_key,
        public_key: o.public_key,
        sign_count: o.sign_count,
      }
    });
    Ok(self.inner.create(&rp_id, options).await.into_napi()?.into())
  }

  #[napi]
  pub async fn get(&self, options: Option<CredentialFilter>) -> Vec<VirtualCredential> {
    let filter = options.map_or_else(Default::default, |o| ferridriver::credentials::CredentialFilter {
      id: o.id,
      rp_id: o.rp_id,
    });
    self.inner.get(filter).await.into_iter().map(Into::into).collect()
  }

  #[napi]
  pub async fn delete(&self, id: String) {
    self.inner.delete(&id).await;
  }

  /// Answer the WebAuthn ceremonies of every page in the context from
  /// these credentials.
  #[napi]
  pub async fn install(&self) -> Result<()> {
    self.inner.install().await.into_napi()
  }
}
