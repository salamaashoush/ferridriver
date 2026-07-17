//! Sandbox network guard (SSRF defense).
//!
//! The scripting sandbox stops disk/process escape; the network was the
//! remaining hole. `NetGuard` is enforced in core (Rust source of truth)
//! so the `request` binding, the global `fetch`, and every plugin
//! `allow.net` tool share one implementation:
//!
//!  * a per-hop host allow-list — checked on the initial URL AND on every
//!    redirect target, so an allowed host can no longer 302 a
//!    net-restricted caller into an internal address;
//!  * a DNS filter that drops cloud-metadata / (optionally) private
//!    resolved addresses, which also defeats DNS rebinding (a public
//!    hostname that resolves to 169.254.169.254);
//!  * scheme pinning (http/https only).
//!
//! Default sandbox posture blocks the cloud-metadata endpoints for every
//! script `fetch`/`request` (no legitimate automation targets them),
//! while loopback/private stays reachable so local test servers keep
//! working unless an operator opts in.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;

/// Boxed error for the custom DNS resolver (`reqwest::dns::Resolving`
/// resolves to `Result<Addrs, BoxError>`).
type BoxErr = Box<dyn std::error::Error + Send + Sync>;

/// Per-request network policy for the scripting sandbox. `Default`
/// (all-false / no allow-list) is inert — non-sandbox callers never set
/// it and keep the original cached-client fast path untouched.
#[derive(Debug, Clone, Default)]
pub struct NetGuard {
  /// Host allow-list (plugin `allow.net`). `None` ⇒ unrestricted host;
  /// `Some` ⇒ default-deny, enforced on the initial URL and every
  /// redirect hop.
  pub allowlist: Option<Arc<[String]>>,
  /// Block the cloud instance-metadata endpoints (169.254.169.254 /
  /// `fd00:ec2::254`) at both the URL and the resolved-address layer.
  pub block_metadata: bool,
  /// Also block loopback / RFC1918 / link-local / ULA / CGNAT. Off by
  /// default so local automation against `127.0.0.1` test servers still
  /// works; an operator opts in.
  pub block_private: bool,
}

impl NetGuard {
  /// Whether this guard changes behaviour at all. When `false` the
  /// caller uses the unguarded cached-client path (zero overhead).
  #[must_use]
  pub fn is_active(&self) -> bool {
    self.allowlist.is_some() || self.block_metadata || self.block_private
  }

  /// The address-family filter this guard needs at the DNS layer, or
  /// `None` when no address filtering applies (allow-list only).
  #[must_use]
  pub(crate) fn dns_filter(&self) -> Option<(bool, bool)> {
    (self.block_metadata || self.block_private).then_some((self.block_metadata, self.block_private))
  }
}

/// Extract the lowercased host (no port, no userinfo) from an absolute
/// URL. `None` for relative/invalid input — callers treat that as a
/// denial when an allow-list is active (fail closed).
#[must_use]
pub fn host_of(url: &str) -> Option<String> {
  let after_scheme = url.split_once("://")?.1;
  let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or(after_scheme);
  let host_port = authority.rsplit_once('@').map_or(authority, |(_, hp)| hp);
  let host = if let Some(stripped) = host_port.strip_prefix('[') {
    stripped.split(']').next().unwrap_or(stripped)
  } else {
    host_port.split(':').next().unwrap_or(host_port)
  };
  (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Match a host against one allow-list entry set: exact, or a
/// leading-wildcard suffix (`*.acme.com` also matches the bare apex
/// `acme.com`).
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

/// Normalize an IPv4-mapped/compatible IPv6 address down to its IPv4
/// form so range checks see the real address.
fn canon_ip(ip: IpAddr) -> IpAddr {
  match ip {
    IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
    v4 @ IpAddr::V4(_) => v4,
  }
}

/// The cloud instance-metadata addresses (AWS/GCP/Azure/OpenStack IMDS,
/// and the AWS IPv6 IMDS). These have no legitimate automation use and
/// are the canonical SSRF target, so they are blocked by default.
fn is_metadata_ip(ip: IpAddr) -> bool {
  match canon_ip(ip) {
    IpAddr::V4(v4) => v4 == Ipv4Addr::new(169, 254, 169, 254),
    IpAddr::V6(v6) => v6 == Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254),
  }
}

/// Loopback / private / link-local / ULA / CGNAT / unspecified — the
/// "internal network" set blocked when `block_private` is on.
fn is_private_ip(ip: IpAddr) -> bool {
  match canon_ip(ip) {
    IpAddr::V4(v4) => {
      v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.octets()[0] == 0
        // RFC 6598 carrier-grade NAT 100.64.0.0/10.
        || (v4.octets()[0] == 100 && (64..=127).contains(&v4.octets()[1]))
    },
    IpAddr::V6(v6) => {
      v6.is_loopback()
        || v6.is_unspecified()
        // Unique-local fc00::/7.
        || (v6.segments()[0] & 0xfe00) == 0xfc00
        // Link-local fe80::/10.
        || (v6.segments()[0] & 0xffc0) == 0xfe80
    },
  }
}

/// `true` if the address must not be connected to under this guard.
fn ip_blocked(ip: IpAddr, block_metadata: bool, block_private: bool) -> bool {
  (block_metadata && is_metadata_ip(ip)) || (block_private && is_private_ip(ip))
}

/// Validate one concrete URL (initial or a redirect target) against the
/// guard: scheme must be http/https, a literal-IP host is range-checked,
/// and the host must satisfy the allow-list. Returns the denial reason.
pub(crate) fn check_url(url: &reqwest::Url, g: &NetGuard) -> Result<(), String> {
  let scheme = url.scheme();
  if scheme != "http" && scheme != "https" {
    return Err(format!(
      "scheme \"{scheme}\" is not permitted by the sandbox network policy"
    ));
  }
  let host = url
    .host_str()
    .ok_or_else(|| "request to a URL with no host is not permitted".to_string())?;
  if let Ok(ip) = host.parse::<IpAddr>()
    && ip_blocked(ip, g.block_metadata, g.block_private)
  {
    return Err(format!("request to blocked address {ip} (sandbox network policy)"));
  }
  if let Some(list) = &g.allowlist
    && !host_allowed(&host.to_ascii_lowercase(), list)
  {
    return Err(format!("request host \"{host}\" is not in allow.net {list:?}"));
  }
  Ok(())
}

/// Pre-flight the initial (already base-resolved) request URL. A
/// parse failure under an active guard is a denial (fail closed).
pub(crate) fn preflight(resolved_url: &str, g: &NetGuard) -> Result<(), String> {
  match reqwest::Url::parse(resolved_url) {
    Ok(u) => check_url(&u, g),
    Err(_) => Err(format!(
      "request to invalid/relative URL \"{resolved_url}\" is not permitted by the sandbox network policy"
    )),
  }
}

/// Custom reqwest DNS resolver that resolves the host normally, then
/// drops any address the guard forbids. Empty after filtering ⇒ the
/// connection is refused. This is what defeats DNS rebinding: a public
/// hostname resolving to a metadata/private address never connects.
pub(crate) struct GuardedResolver {
  pub block_metadata: bool,
  pub block_private: bool,
}

impl reqwest::dns::Resolve for GuardedResolver {
  fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
    let host = name.as_str().to_string();
    let (bm, bp) = (self.block_metadata, self.block_private);
    Box::pin(async move {
      let lookup = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<SocketAddr>> {
        Ok((host.as_str(), 0u16).to_socket_addrs()?.collect())
      })
      .await;
      let addrs = match lookup {
        Ok(Ok(a)) => a,
        Ok(Err(e)) => return Err(Box::new(e) as BoxErr),
        Err(e) => return Err(Box::new(e) as BoxErr),
      };
      let kept: Vec<SocketAddr> = addrs.into_iter().filter(|sa| !ip_blocked(sa.ip(), bm, bp)).collect();
      if kept.is_empty() {
        return Err("all resolved addresses blocked by sandbox network policy".into());
      }
      Ok(Box::new(kept.into_iter()) as reqwest::dns::Addrs)
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn host_of_ignores_userinfo_and_port() {
    assert_eq!(host_of("https://allowed.com/x").as_deref(), Some("allowed.com"));
    // userinfo must not let an attacker spoof the host.
    assert_eq!(host_of("https://allowed.com@evil.com/x").as_deref(), Some("evil.com"));
    assert_eq!(host_of("http://[::1]:8080/").as_deref(), Some("::1"));
    assert_eq!(host_of("/relative"), None);
  }

  #[test]
  fn host_allowlist_exact_and_wildcard() {
    let net = ["api.acme.com".to_string(), "*.cdn.com".to_string()];
    assert!(host_allowed("api.acme.com", &net));
    assert!(host_allowed("cdn.com", &net)); // apex
    assert!(host_allowed("a.cdn.com", &net));
    assert!(!host_allowed("evilcdn.com", &net));
    assert!(!host_allowed("acme.com", &net));
  }

  #[test]
  fn metadata_addresses_classified() {
    assert!(is_metadata_ip("169.254.169.254".parse().unwrap()));
    // IPv4-mapped IPv6 must normalise so it cannot smuggle past.
    assert!(is_metadata_ip("::ffff:169.254.169.254".parse().unwrap()));
    assert!(is_metadata_ip("fd00:ec2::254".parse().unwrap()));
    assert!(!is_metadata_ip("93.184.216.34".parse().unwrap()));
  }

  #[test]
  fn private_ranges_classified() {
    for ip in [
      "127.0.0.1",
      "10.0.0.1",
      "192.168.1.1",
      "172.16.0.1",
      "169.254.0.1",
      "100.64.0.1",
      "::1",
      "fe80::1",
      "fc00::1",
    ] {
      assert!(is_private_ip(ip.parse().unwrap()), "{ip} should be private");
    }
    assert!(!is_private_ip("8.8.8.8".parse().unwrap()));
  }

  #[test]
  fn check_url_blocks_metadata_by_default_keeps_loopback() {
    let g = NetGuard {
      allowlist: None,
      block_metadata: true,
      block_private: false,
    };
    assert!(check_url(&reqwest::Url::parse("http://169.254.169.254/").unwrap(), &g).is_err());
    // Loopback stays reachable so local automation/test servers work.
    assert!(check_url(&reqwest::Url::parse("http://127.0.0.1:9/").unwrap(), &g).is_ok());
    // Non-http(s) scheme rejected.
    assert!(check_url(&reqwest::Url::parse("file:///etc/passwd").unwrap(), &g).is_err());
  }

  #[test]
  fn check_url_enforces_allowlist_on_any_url() {
    let g = NetGuard {
      allowlist: Some(Arc::from(["allowed.com".to_string()])),
      block_metadata: true,
      block_private: false,
    };
    assert!(check_url(&reqwest::Url::parse("https://allowed.com/x").unwrap(), &g).is_ok());
    // This is the per-hop check that closes the redirect SSRF bypass:
    // the same function the manual redirect loop calls on every hop.
    assert!(check_url(&reqwest::Url::parse("https://evil.com/x").unwrap(), &g).is_err());
  }

  #[test]
  fn preflight_fails_closed_on_unparseable_url() {
    let g = NetGuard {
      allowlist: Some(Arc::from(["allowed.com".to_string()])),
      block_metadata: true,
      block_private: false,
    };
    assert!(preflight("not a url", &g).is_err());
  }

  #[test]
  fn inert_guard_is_not_active() {
    assert!(!NetGuard::default().is_active());
    assert!(
      NetGuard {
        block_metadata: true,
        ..Default::default()
      }
      .is_active()
    );
  }
}
