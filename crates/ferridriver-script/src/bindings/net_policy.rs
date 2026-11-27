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
//! the declared list, over the realm's container. Both `SessionFetch`
//! (`fetch`) and `HttpClientJs` (`request` / `APIRequestContext`) share
//! `NetAuthority` so policy resolution and attenuation exist once.

use std::sync::{Arc, Mutex};

use ferridriver::http_client::{HttpClient, NetGuard, NetPolicy};
use ferrijs::Permissions;
use ferrijs::fetch::{FetchBackend, FetchFuture, FetchRequest};
use rquickjs::Ctx;

/// Shared network-authority handle carried by both `SessionFetch` (`fetch`)
/// and `HttpClientJs` (`request` / `APIRequestContext`).
///
/// `None` means the binding answers to the realm's permission container
/// alone; `Some(declared)` layers an extension tool's `allow.net` list
/// over the realm container so revoking at realm level still narrows it.
#[derive(Clone, Default)]
pub(crate) struct NetAuthority {
  attenuation: Option<Arc<Permissions>>,
}

impl NetAuthority {
  #[must_use]
  pub(crate) const fn realm() -> Self {
    Self { attenuation: None }
  }

  #[must_use]
  pub(crate) fn attenuated(declared: Arc<Permissions>) -> Self {
    Self {
      attenuation: Some(declared),
    }
  }

  /// Build an attenuated authority from a manifest `allow.net` host list.
  ///
  /// # Errors
  ///
  /// Returns an error string when any entry is not a valid host rule.
  pub(crate) fn from_net_list(net: &[String]) -> Result<Self, String> {
    Ok(Self::attenuated(Arc::new(Permissions::none().allow_net(net)?)))
  }

  /// Combine this authority with an explicit realm policy (used by
  /// `FetchBackend::net_policy`).
  #[must_use]
  pub(crate) fn over(&self, realm: Arc<dyn NetPolicy>) -> Arc<dyn NetPolicy> {
    match &self.attenuation {
      Some(declared) => Arc::new(Over {
        realm,
        declared: Arc::clone(declared),
      }),
      None => realm,
    }
  }

  /// Resolve the effective [`NetPolicy`] for `ctx` synchronously at call time.
  #[must_use]
  pub(crate) fn policy(&self, ctx: &Ctx<'_>) -> Option<Arc<dyn NetPolicy>> {
    match (realm_policy(ctx), &self.attenuation) {
      (Some(realm), Some(declared)) => Some(Arc::new(Over {
        realm,
        declared: Arc::clone(declared),
      })),
      (Some(realm), None) => Some(realm),
      (None, Some(declared)) => Some(Arc::clone(declared) as Arc<dyn NetPolicy>),
      (None, None) => None,
    }
  }

  /// Build the engine [`NetGuard`] for `ctx` synchronously at call time.
  #[must_use]
  pub(crate) fn guard(&self, ctx: &Ctx<'_>) -> NetGuard {
    guard(self.policy(ctx))
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
/// context-bound client where the one before it had none.
pub struct SessionFetch {
  client: Mutex<Arc<HttpClient>>,
  authority: NetAuthority,
}

impl SessionFetch {
  #[must_use]
  pub fn new(client: Arc<HttpClient>) -> Self {
    Self {
      client: Mutex::new(client),
      authority: NetAuthority::realm(),
    }
  }

  /// The same client, refusing every host outside `declared`.
  #[must_use]
  pub(crate) fn attenuated(&self, declared: Arc<Permissions>) -> Self {
    Self {
      client: Mutex::new(self.client()),
      authority: NetAuthority::attenuated(declared),
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
    self.authority.over(realm)
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
