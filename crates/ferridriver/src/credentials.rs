//! Playwright's `browserContext.credentials`: a virtual `WebAuthn`
//! authenticator holding passkeys for the context.
//!
//! [`Credentials::install`] answers the page's `navigator.credentials`
//! ceremonies: the vendored `webAuthn.ts` overrides `create()` / `get()`
//! and asks this registry, through the `__pwWebAuthnBinding` binding, to
//! make or use a credential. Keys are P-256, attestation is `none`, and
//! every assertion bumps the credential's signature counter — the same
//! authenticator Playwright's `server/credentials.ts` implements.

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use ring::rand::SecureRandom as _;
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair as _};
use sha2::Digest as _;

use crate::error::{FerriError, Result};

const BINDING: &str = "__pwWebAuthnBinding";
const WEBAUTHN_JS: &str = include_str!("injected/dist/webauthn.min.js");

/// DER `SubjectPublicKeyInfo` header for an uncompressed P-256 point:
/// `id-ecPublicKey` with the `prime256v1` curve, then the BIT STRING
/// that the 65-byte point completes.
const P256_SPKI_PREFIX: [u8; 26] = [
  0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce,
  0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
];

/// A passkey as Playwright hands it out: every binary field base64url.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VirtualCredential {
  pub id: String,
  pub rp_id: String,
  pub user_handle: String,
  /// DER PKCS#8.
  pub private_key: String,
  /// DER `SubjectPublicKeyInfo`.
  pub public_key: String,
  /// Storage state saved before the counter existed has none; it starts
  /// from zero.
  #[serde(default)]
  pub sign_count: u32,
}

/// `credentials.create(rpId, options?)`: any field left out is generated.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CredentialCreateOptions {
  pub id: Option<String>,
  pub user_handle: Option<String>,
  pub private_key: Option<String>,
  pub public_key: Option<String>,
  /// Wider than `u32` so an out-of-range value reaches the range check
  /// and its message.
  pub sign_count: Option<i64>,
}

/// `credentials.get(options?)`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CredentialFilter {
  pub id: Option<String>,
  pub rp_id: Option<String>,
}

#[derive(Debug, Clone)]
struct Record {
  credential: VirtualCredential,
  resident: bool,
}

/// One context's authenticator: its passkeys and, once installed, what
/// answers the page.
#[derive(Default)]
pub(crate) struct CredentialStore {
  records: Mutex<Vec<Record>>,
  installed: tokio::sync::Mutex<Option<(crate::disposable::Disposable, crate::disposable::Disposable)>>,
}

impl CredentialStore {
  fn records(&self) -> std::sync::MutexGuard<'_, Vec<Record>> {
    self.records.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  fn insert(&self, record: Record) {
    let mut records = self.records();
    records.retain(|r| r.credential.id != record.credential.id);
    records.push(record);
  }

  fn handle(&self, payload: &serde_json::Value) -> serde_json::Value {
    let outcome = match payload.get("type").and_then(serde_json::Value::as_str) {
      Some("create") => self.handle_create(payload),
      Some("get") => self.handle_get(payload),
      _ => Err(("NotAllowedError", "Unknown WebAuthn request".to_string())),
    };
    outcome.unwrap_or_else(|(name, message)| serde_json::json!({ "ok": false, "name": name, "message": message }))
  }

  fn handle_create(&self, req: &serde_json::Value) -> std::result::Result<serde_json::Value, (&'static str, String)> {
    let not_allowed = |e: FerriError| ("NotAllowedError", e.to_string());
    let origin = text(req, "origin");
    let rp_id = req
      .pointer("/rp/id")
      .and_then(serde_json::Value::as_str)
      .map_or_else(|| host_of(&origin), str::to_string);
    if let Some(excluded) = req.get("excludeCredentials").and_then(serde_json::Value::as_array) {
      let records = self.records();
      if excluded
        .iter()
        .filter_map(|d| d.get("id").and_then(serde_json::Value::as_str))
        .any(|id| records.iter().any(|r| r.credential.id == id))
      {
        return Err(("InvalidStateError", "Credential excluded".into()));
      }
    }
    let (pkcs8, point) = generate_key().map_err(not_allowed)?;
    let id_bytes = random_bytes(16).map_err(not_allowed)?;
    let id = b64url(&id_bytes);
    let resident = matches!(
      req.get("residentKey").and_then(serde_json::Value::as_str),
      Some("required" | "preferred")
    );
    let credential = VirtualCredential {
      id: id.clone(),
      rp_id: rp_id.clone(),
      user_handle: req
        .pointer("/user/id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string(),
      private_key: b64url(&pkcs8),
      public_key: b64url(&spki(&point)),
      sign_count: 0,
    };
    let client_data = client_data("webauthn.create", req, &origin);
    let mut auth_data = authenticator_data(&rp_id, 0x01 | 0x04 | 0x40, 0);
    // Attested credential data: the all-zero AAGUID, the id, the key.
    auth_data.extend_from_slice(&[0; 16]);
    auth_data.extend_from_slice(&u16::try_from(id_bytes.len()).unwrap_or(16).to_be_bytes());
    auth_data.extend_from_slice(&id_bytes);
    auth_data.extend_from_slice(&cose_es256_key(&point));
    let attestation = cbor_map(&[
      (cbor_text("fmt"), cbor_text("none")),
      (cbor_text("attStmt"), cbor_map(&[])),
      (cbor_text("authData"), cbor_bytes(&auth_data)),
    ]);
    let public_key = credential.public_key.clone();
    self.insert(Record { credential, resident });
    Ok(serde_json::json!({
      "ok": true,
      "id": id,
      "clientDataJSON": b64url(&client_data),
      "attestationObject": b64url(&attestation),
      "authenticatorData": b64url(&auth_data),
      "publicKey": public_key,
    }))
  }

  fn handle_get(&self, req: &serde_json::Value) -> std::result::Result<serde_json::Value, (&'static str, String)> {
    let origin = text(req, "origin");
    let rp_id = req
      .get("rpId")
      .and_then(serde_json::Value::as_str)
      .filter(|id| !id.is_empty())
      .map_or_else(|| host_of(&origin), str::to_string);
    let allowed: Vec<&str> = req
      .get("allowCredentials")
      .and_then(serde_json::Value::as_array)
      .map(|list| {
        list
          .iter()
          .filter_map(|d| d.get("id").and_then(serde_json::Value::as_str))
          .collect()
      })
      .unwrap_or_default();
    let mut records = self.records();
    let candidate = if allowed.is_empty() {
      records.iter_mut().find(|r| r.credential.rp_id == rp_id && r.resident)
    } else {
      records
        .iter_mut()
        .find(|r| r.credential.rp_id == rp_id && allowed.contains(&r.credential.id.as_str()))
    };
    let Some(candidate) = candidate else {
      return Err(("NotAllowedError", "No matching credential".into()));
    };
    candidate.credential.sign_count = candidate.credential.sign_count.wrapping_add(1);
    let client_data = client_data("webauthn.get", req, &origin);
    let auth_data = authenticator_data(&rp_id, 0x01 | 0x04, candidate.credential.sign_count);
    let mut signed = auth_data.clone();
    signed.extend_from_slice(&sha2::Sha256::digest(&client_data));
    let signature = sign(&candidate.credential.private_key, &signed).map_err(|e| ("NotAllowedError", e.to_string()))?;
    Ok(serde_json::json!({
      "ok": true,
      "id": candidate.credential.id,
      "clientDataJSON": b64url(&client_data),
      "authenticatorData": b64url(&auth_data),
      "signature": b64url(&signature),
      "userHandle": (!candidate.credential.user_handle.is_empty()).then(|| candidate.credential.user_handle.clone()),
    }))
  }
}

/// `browserContext.credentials`.
#[derive(Clone)]
pub struct Credentials {
  context: crate::context::ContextRef,
}

impl Credentials {
  pub(crate) fn new(context: crate::context::ContextRef) -> Self {
    Self { context }
  }

  async fn store(&self) -> Arc<CredentialStore> {
    self.context.credential_store().await
  }

  /// Playwright: `credentials.create(rpId, options?)`. Seeds a
  /// discoverable credential for `rp_id`, generating whatever `options`
  /// leaves out.
  ///
  /// # Errors
  ///
  /// Returns an error if `signCount` is out of range or a key cannot be
  /// generated.
  pub async fn create(&self, rp_id: &str, options: CredentialCreateOptions) -> Result<VirtualCredential> {
    let requested = options.sign_count.unwrap_or(0);
    let sign_count = u32::try_from(requested).map_err(|_| {
      FerriError::invalid_argument(
        "signCount",
        format!("signCount must be between 0 and 4294967295, got {requested}"),
      )
    })?;
    let (private_key, public_key) =
      if let (Some(private_key), Some(public_key)) = (options.private_key, options.public_key) {
        (private_key, public_key)
      } else {
        let (pkcs8, point) = generate_key()?;
        (b64url(&pkcs8), b64url(&spki(&point)))
      };
    let credential = VirtualCredential {
      id: match options.id.filter(|id| !id.is_empty()) {
        Some(id) => id,
        None => b64url(&random_bytes(16)?),
      },
      rp_id: rp_id.to_string(),
      user_handle: match options.user_handle.filter(|handle| !handle.is_empty()) {
        Some(handle) => handle,
        None => b64url(&random_bytes(16)?),
      },
      private_key,
      public_key,
      sign_count,
    };
    self.store().await.insert(Record {
      credential: credential.clone(),
      resident: true,
    });
    Ok(credential)
  }

  /// Playwright: `credentials.get(options?)`.
  pub async fn get(&self, filter: CredentialFilter) -> Vec<VirtualCredential> {
    self
      .store()
      .await
      .records()
      .iter()
      .filter(|r| filter.rp_id.as_ref().is_none_or(|rp| &r.credential.rp_id == rp))
      .filter(|r| filter.id.as_ref().is_none_or(|id| &r.credential.id == id))
      .map(|r| r.credential.clone())
      .collect()
  }

  /// Playwright: `credentials.delete(id)`.
  pub async fn delete(&self, id: &str) {
    self.store().await.records().retain(|r| r.credential.id != id);
  }

  /// Playwright: `credentials.install()`. From now on every page of the
  /// context, present and future, has its `WebAuthn` ceremonies answered
  /// from these credentials.
  ///
  /// # Errors
  ///
  /// Returns an error if the binding or the script cannot be installed.
  pub async fn install(&self) -> Result<()> {
    let store = self.store().await;
    let mut installed = store.installed.lock().await;
    if installed.is_some() {
      return Ok(());
    }
    let answering = Arc::clone(&store);
    let binding: crate::events::ExposedBinding = Arc::new(move |_source, args| {
      let reply = answering.handle(args.first().unwrap_or(&serde_json::Value::Null));
      Box::pin(async move { Ok(reply.into()) })
    });
    let binding = self.context.expose_binding(BINDING, binding).await?;
    let script = self.context.add_init_script_source(WEBAUTHN_JS.to_string()).await?;
    *installed = Some((binding, script));
    drop(installed);
    for page in self.context.discover_pages().await? {
      for frame in page.frames() {
        let _ = frame
          .evaluate_untraced(WEBAUTHN_JS, crate::protocol::SerializedArgument::default(), Some(false))
          .await;
      }
    }
    Ok(())
  }

  /// Replace the credentials with `state`'s, as restoring a storage state
  /// does: installed when there are any, uninstalled when there are none.
  pub(crate) async fn restore(&self, credentials: Vec<VirtualCredential>) -> Result<()> {
    let store = self.store().await;
    store.records().clear();
    if credentials.is_empty() {
      if let Some((binding, script)) = store.installed.lock().await.take() {
        binding.dispose().await?;
        script.dispose().await?;
      }
      return Ok(());
    }
    for credential in credentials {
      store.insert(Record {
        credential,
        resident: true,
      });
    }
    self.install().await
  }
}

fn text(req: &serde_json::Value, key: &str) -> String {
  req
    .get(key)
    .and_then(serde_json::Value::as_str)
    .unwrap_or_default()
    .to_string()
}

fn host_of(origin: &str) -> String {
  reqwest::Url::parse(origin)
    .ok()
    .and_then(|url| url.host_str().map(str::to_string))
    .unwrap_or_default()
}

fn client_data(kind: &str, req: &serde_json::Value, origin: &str) -> Vec<u8> {
  serde_json::json!({
    "type": kind,
    "challenge": text(req, "challenge"),
    "origin": origin,
    "crossOrigin": false,
  })
  .to_string()
  .into_bytes()
}

fn authenticator_data(rp_id: &str, flags: u8, sign_count: u32) -> Vec<u8> {
  let mut data = sha2::Sha256::digest(rp_id.as_bytes()).to_vec();
  data.push(flags);
  data.extend_from_slice(&sign_count.to_be_bytes());
  data
}

fn b64url(bytes: &[u8]) -> String {
  base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn random_bytes(len: usize) -> Result<Vec<u8>> {
  let mut bytes = vec![0; len];
  ring::rand::SystemRandom::new()
    .fill(&mut bytes)
    .map_err(|_| FerriError::backend("WebAuthn: no system randomness"))?;
  Ok(bytes)
}

/// A fresh P-256 key: its PKCS#8 document and its uncompressed point.
fn generate_key() -> Result<(Vec<u8>, Vec<u8>)> {
  let rng = ring::rand::SystemRandom::new();
  let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng)
    .map_err(|_| FerriError::backend("WebAuthn: cannot generate a P-256 key"))?;
  let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
    .map_err(|_| FerriError::backend("WebAuthn: cannot read the generated key"))?;
  Ok((pkcs8.as_ref().to_vec(), pair.public_key().as_ref().to_vec()))
}

fn spki(point: &[u8]) -> Vec<u8> {
  let mut der = P256_SPKI_PREFIX.to_vec();
  der.extend_from_slice(point);
  der
}

fn sign(private_key: &str, message: &[u8]) -> Result<Vec<u8>> {
  let der = base64::engine::general_purpose::URL_SAFE_NO_PAD
    .decode(private_key.trim_end_matches('='))
    .map_err(|e| FerriError::invalid_argument("privateKey", format!("not base64url: {e}")))?;
  let rng = ring::rand::SystemRandom::new();
  let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &der, &rng)
    .map_err(|e| FerriError::invalid_argument("privateKey", format!("not a P-256 PKCS#8 key: {e}")))?;
  let signature = pair
    .sign(&rng, message)
    .map_err(|_| FerriError::backend("WebAuthn: signing failed"))?;
  Ok(signature.as_ref().to_vec())
}

// The CBOR subset COSE keys and the attestation object need.

fn cbor_head(major: u8, value: usize) -> Vec<u8> {
  let m = major << 5;
  match value {
    0..24 => vec![m | u8::try_from(value).unwrap_or(0)],
    24..0x100 => vec![m | 0x18, u8::try_from(value).unwrap_or(0)],
    0x100..0x1_0000 => {
      let mut head = vec![m | 0x19];
      head.extend_from_slice(&u16::try_from(value).unwrap_or(0).to_be_bytes());
      head
    },
    _ => {
      let mut head = vec![m | 0x1a];
      head.extend_from_slice(&u32::try_from(value).unwrap_or(u32::MAX).to_be_bytes());
      head
    },
  }
}

fn cbor_uint(value: usize) -> Vec<u8> {
  cbor_head(0, value)
}

/// A negative integer `-1 - magnitude`.
fn cbor_nint(magnitude: usize) -> Vec<u8> {
  cbor_head(1, magnitude)
}

fn cbor_bytes(bytes: &[u8]) -> Vec<u8> {
  let mut out = cbor_head(2, bytes.len());
  out.extend_from_slice(bytes);
  out
}

fn cbor_text(text: &str) -> Vec<u8> {
  let mut out = cbor_head(3, text.len());
  out.extend_from_slice(text.as_bytes());
  out
}

fn cbor_map(entries: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
  let mut out = cbor_head(5, entries.len());
  for (key, value) in entries {
    out.extend_from_slice(key);
    out.extend_from_slice(value);
  }
  out
}

/// The COSE `ES256` key for an uncompressed P-256 point.
fn cose_es256_key(point: &[u8]) -> Vec<u8> {
  let (x, y) = point[1..].split_at(32);
  cbor_map(&[
    (cbor_uint(1), cbor_uint(2)), // kty: EC2
    (cbor_uint(3), cbor_nint(6)), // alg: -7, ES256
    (cbor_nint(0), cbor_uint(1)), // crv: P-256
    (cbor_nint(1), cbor_bytes(x)),
    (cbor_nint(2), cbor_bytes(y)),
  ])
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn generated_keys_round_trip_through_signing() {
    let (pkcs8, point) = generate_key().unwrap();
    assert_eq!(point.len(), 65);
    assert_eq!(spki(&point).len(), 91);
    let signature = sign(&b64url(&pkcs8), b"message").unwrap();
    ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_ASN1, &point)
      .verify(b"message", &signature)
      .unwrap();
  }

  #[test]
  fn cbor_encodes_cose_key_and_attestation_heads() {
    assert_eq!(cbor_nint(6), vec![0x26]);
    assert_eq!(cbor_head(2, 32), vec![0x58, 0x20]);
    let key = cose_es256_key(&[4; 65]);
    assert_eq!(&key[..7], &[0xa5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01]);
  }

  #[test]
  fn an_assertion_bumps_the_counter_and_needs_a_matching_credential() {
    let store = CredentialStore::default();
    let (pkcs8, point) = generate_key().unwrap();
    store.insert(Record {
      credential: VirtualCredential {
        id: "cred".into(),
        rp_id: "example.com".into(),
        user_handle: "user".into(),
        private_key: b64url(&pkcs8),
        public_key: b64url(&spki(&point)),
        sign_count: 41,
      },
      resident: true,
    });
    let reply = store.handle(
      &serde_json::json!({ "type": "get", "origin": "https://example.com", "challenge": "c", "rpId": "example.com" }),
    );
    assert_eq!(reply["ok"], true);
    let auth = base64::engine::general_purpose::URL_SAFE_NO_PAD
      .decode(reply["authenticatorData"].as_str().unwrap())
      .unwrap();
    assert_eq!(u32::from_be_bytes(auth[33..37].try_into().unwrap()), 42);
    let missing = store.handle(
      &serde_json::json!({ "type": "get", "origin": "https://other.com", "challenge": "c", "rpId": "other.com" }),
    );
    assert_eq!(missing["name"], "NotAllowedError");
  }
}
