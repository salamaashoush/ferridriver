//! Network authority in a session, all of it the runtime's model.
//!
//! The realm's policy is ferrijs's: one container per session VM,
//! narrowing only. Every `fetch` and every `request` call in the realm
//! answers to it, on the first URL and every redirect hop. There is no
//! second policy layered on top for extension tools: privilege is per
//! realm, as it is in Deno, Node and workerd, and an extension that
//! must be confined below the session it is loaded into gets a session
//! of its own.
//!
//! What a tool's manifest `allow.net` still means is attenuation, the
//! object-capability shape: the `request` and `fetch` objects handed to
//! the handler as `ctx.request` / `ctx.fetch` refuse every host outside
//! the declared list, over the realm's container. A handler that uses
//! what it was given is bound by it; the globals it can also reach are
//! bound by the realm.

use std::sync::{Arc, Mutex};

use ferridriver::http_client::{HttpClient, NetGuard, NetPolicy};
use ferrijs::Permissions;
use ferrijs::fetch::{FetchBackend, FetchFuture, FetchRequest};
use rquickjs::Ctx;

/// `declared` over the realm's policy: the container first (so a later
/// realm-level revoke still binds a handler holding an older
/// capability), the declared list second.
pub(crate) fn attenuated(ctx: &Ctx<'_>, declared: Arc<Permissions>) -> Arc<dyn NetPolicy> {
  match realm_policy(ctx) {
    Some(realm) => Arc::new(Over { realm, declared }),
    None => declared as Arc<dyn NetPolicy>,
  }
}

/// Match a host against one allow-list entry set: exact, or a
/// leading-wildcard suffix (`*.acme.com` also matches the bare apex
/// `acme.com`). How the operator ceiling on a manifest's `allow.net`
/// is judged at registration; the engine's per-hop check is the
/// runtime's `NetRule`, which reads the same syntax.
#[must_use]
pub fn host_allowed(host: &str, net: &[String]) -> bool {
  net.iter().any(|p| {
    if p == host {
      return true;
    }
    if let Some(suffix) = p.strip_prefix("*.") {
      return host == suffix || host.ends_with(&format!(".{suffix}"));
    }
    false
  })
}

/// The engine guard for a request: `policy` (the realm's container, or
/// an attenuation over it), the cloud-metadata endpoints blocked for
/// every script request regardless of grant (closes the default-open
/// SSRF), loopback left reachable so local test servers keep working.
pub(crate) fn guard(policy: Option<Arc<dyn NetPolicy>>) -> NetGuard {
  NetGuard {
    policy,
    block_metadata: true,
    block_private: false,
  }
}

/// The realm's own policy, for a binding building a guard.
pub(crate) fn realm_policy(ctx: &Ctx<'_>) -> Option<Arc<dyn NetPolicy>> {
  ferrijs::std::permissions::container(ctx).map(|c| c as Arc<dyn NetPolicy>)
}

/// `fetch` over the session's HTTP context: the same core the
/// Playwright-style `request` binding wraps, so one net policy applies
/// to both. The client is swapped per call, because a run can carry a
/// context-bound client where the one before it had none. `attenuation`
/// is set on the copy handed to a tool handler as `ctx.fetch`.
pub struct SessionFetch {
  client: Mutex<Arc<HttpClient>>,
  attenuation: Option<Arc<Permissions>>,
}

impl SessionFetch {
  #[must_use]
  pub fn new(client: Arc<HttpClient>) -> Self {
    Self {
      client: Mutex::new(client),
      attenuation: None,
    }
  }

  /// The same client, refusing every host outside `declared`.
  #[must_use]
  pub(crate) fn attenuated(&self, declared: Arc<Permissions>) -> Self {
    Self {
      client: Mutex::new(self.client()),
      attenuation: Some(declared),
    }
  }

  pub fn set_client(&self, client: Arc<HttpClient>) {
    *self.client.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = client;
  }

  pub(crate) fn client(&self) -> Arc<HttpClient> {
    Arc::clone(&self.client.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
  }
}

impl FetchBackend for SessionFetch {
  fn fetch(&self, request: FetchRequest) -> FetchFuture<'_> {
    let client = self.client();
    Box::pin(async move {
      client
        .send_whatwg(ferridriver::http_client::WhatwgRequest {
          url: request.url,
          method: request.method,
          headers: request.headers,
          body: request.body,
          redirect: request.redirect,
          credentials: request.credentials,
          net_guard: request.net_guard,
          timeout: request.timeout,
        })
        .await
    })
  }

  fn net_policy(&self, _ctx: &Ctx<'_>, realm: Arc<dyn NetPolicy>) -> Arc<dyn NetPolicy> {
    match &self.attenuation {
      Some(declared) => Arc::new(Over {
        realm,
        declared: Arc::clone(declared),
      }),
      None => realm,
    }
  }
}

/// The realm's policy, then a declared list.
#[derive(Debug)]
struct Over {
  realm: Arc<dyn NetPolicy>,
  declared: Arc<Permissions>,
}

impl NetPolicy for Over {
  fn check(&self, host: &str, port: Option<u16>) -> Result<(), ferrijs::Denied> {
    self.realm.check(host, port)?;
    self.declared.check_net(host, port)
  }
}
