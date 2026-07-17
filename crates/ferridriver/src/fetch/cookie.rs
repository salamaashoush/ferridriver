//! RFC 6265 cookie parsing and matching for the context-bound request
//! path (`page.request` / `context.request`). Ported from Playwright's
//! `server/cookieStore.ts` + `server/fetch.ts` cookie handling: the
//! browser context is the jar, so the outgoing `Cookie` header is
//! assembled here from `context.cookies()` and every hop's `Set-Cookie`
//! is parsed back with the same defaults Playwright applies.

use crate::backend::CookieData;

/// RFC 6265 upper bound Playwright clamps cookie expiry to
/// (`server/network.ts::kMaxCookieExpiresDateInSeconds`).
const MAX_COOKIE_EXPIRES_SECONDS: f64 = 253_402_300_799.0;

/// Secure-cookie carve-out: Playwright sends `Secure` cookies over
/// plain http to localhost names (`server/network.ts::isLocalHostname`).
fn is_local_hostname(hostname: &str) -> bool {
  hostname == "localhost" || hostname.ends_with(".localhost")
}

/// RFC 6265 §5.1.3 domain-match as Playwright implements it
/// (`server/cookieStore.ts::domainMatches`): exact host, or a
/// dot-prefixed cookie domain suffix-matching the host.
fn cookie_domain_matches(hostname: &str, domain: &str) -> bool {
  if hostname == domain {
    return true;
  }
  if !domain.starts_with('.') {
    return false;
  }
  format!(".{hostname}").ends_with(domain)
}

/// RFC 6265 §5.1.4 path-match (`server/cookieStore.ts::pathMatches`).
fn cookie_path_matches(request_path: &str, cookie_path: &str) -> bool {
  if request_path == cookie_path {
    return true;
  }
  let mut value = request_path.to_string();
  if !value.ends_with('/') {
    value.push('/');
  }
  let mut path = cookie_path.to_string();
  if !path.ends_with('/') {
    path.push('/');
  }
  value.starts_with(&path)
}

/// Whether a context cookie is sent on a request to `url`
/// (`server/cookieStore.ts::Cookie.matches`, plus the expiry prune).
/// `expires`: `None` / negative = session cookie (never expires here);
/// `>= 0` = absolute epoch seconds, pruned when in the past.
pub(crate) fn cookie_matches_url(cookie: &CookieData, url: &reqwest::Url) -> bool {
  let hostname = url.host_str().unwrap_or("");
  if cookie.secure && url.scheme() != "https" && !is_local_hostname(hostname) {
    return false;
  }
  if !cookie_domain_matches(hostname, &cookie.domain) {
    return false;
  }
  if !cookie_path_matches(url.path(), &cookie.path) {
    return false;
  }
  if let Some(expires) = cookie.expires
    && expires >= 0.0
    && expires
      < std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
  {
    return false;
  }
  true
}

/// Parse one `Set-Cookie` header value into a [`CookieData`], porting
/// Playwright's `parseRawCookie` (`server/cookieStore.ts:131`) +
/// `parseCookie` defaults (`server/fetch.ts:889`). Returns `None` for an
/// empty header.
fn parse_raw_set_cookie(header: &str) -> Option<CookieData> {
  let mut pairs = header.split(';').filter(|s| !s.trim().is_empty()).map(|p| {
    p.split_once('=').map_or_else(
      || (p.trim().to_string(), String::new()),
      |(k, v)| (k.trim().to_string(), v.trim().to_string()),
    )
  });
  let (name, value) = pairs.next()?;
  let mut cookie = CookieData {
    name,
    value,
    domain: String::new(),
    path: String::new(),
    secure: false,
    http_only: false,
    expires: None,
    // Unspecified SameSite behaves as Lax (fetch.ts:900 comment); Playwright
    // stores the default explicitly.
    same_site: Some(crate::backend::SameSite::Lax),
    url: None,
  };
  for (attr, attr_value) in pairs {
    match attr.to_ascii_lowercase().as_str() {
      "expires" => {
        // RFC 6265 §5.2.1: unparseable dates are ignored; past dates clamp
        // to the earliest representable time.
        if let Ok(when) = httpdate::parse_http_date(&attr_value) {
          let secs = when
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
          cookie.expires = Some(secs.min(MAX_COOKIE_EXPIRES_SECONDS));
        }
      },
      "max-age" => {
        // RFC 6265 §5.2.2: non-positive delta = earliest representable time.
        if let Ok(delta) = attr_value.parse::<i64>() {
          if delta <= 0 {
            cookie.expires = Some(0.0);
          } else {
            let now = std::time::SystemTime::now()
              .duration_since(std::time::UNIX_EPOCH)
              .map_or(0.0, |d| d.as_secs_f64());
            // u32 seconds (~136 years) is beyond the RFC clamp anyway;
            // saturating keeps the arithmetic lossless for clippy.
            let delta = f64::from(u32::try_from(delta).unwrap_or(u32::MAX));
            cookie.expires = Some((now + delta).min(MAX_COOKIE_EXPIRES_SECONDS));
          }
        }
      },
      "domain" => {
        let mut domain = attr_value.to_ascii_lowercase();
        // Playwright normalises a dotted-but-not-dot-prefixed domain to its
        // dot-prefixed (subdomain-matching) form.
        if !domain.is_empty() && !domain.starts_with('.') && domain.contains('.') {
          domain.insert(0, '.');
        }
        cookie.domain = domain;
      },
      "path" => cookie.path = attr_value,
      "secure" => cookie.secure = true,
      "httponly" => cookie.http_only = true,
      "samesite" => {
        cookie.same_site = match attr_value.to_ascii_lowercase().as_str() {
          "none" => Some(crate::backend::SameSite::None),
          "strict" => Some(crate::backend::SameSite::Strict),
          "lax" => Some(crate::backend::SameSite::Lax),
          _ => cookie.same_site,
        };
      },
      _ => {},
    }
  }
  Some(cookie)
}

/// Parse every `Set-Cookie` header on a response into browser-ready
/// cookies, applying the RFC 6265 §5.2.3/§5.2.4 domain/path defaults
/// relative to the response URL and dropping cookies whose declared
/// domain does not cover it (`server/fetch.ts::_parseSetCookieHeader`).
pub(crate) fn parse_set_cookie_headers(
  response_url: &reqwest::Url,
  headers: &reqwest::header::HeaderMap,
) -> Vec<CookieData> {
  let hostname = response_url.host_str().unwrap_or("");
  // RFC 6265 §5.1.4 default-path: directory of the request path.
  let path = response_url.path();
  let default_path = {
    let trimmed = path.strip_prefix('/').unwrap_or(path);
    let segments: Vec<&str> = trimmed.split('/').collect();
    format!("/{}", segments[..segments.len().saturating_sub(1)].join("/"))
  };
  let mut cookies = Vec::new();
  for value in headers.get_all(reqwest::header::SET_COOKIE) {
    let Ok(raw) = value.to_str() else { continue };
    let Some(mut cookie) = parse_raw_set_cookie(raw) else {
      continue;
    };
    if cookie.domain.is_empty() {
      // Host-only cookie: bare response hostname, exact-match semantics.
      cookie.domain = hostname.to_string();
    }
    if !cookie_domain_matches(hostname, &cookie.domain) {
      continue;
    }
    if cookie.path.is_empty() || !cookie.path.starts_with('/') {
      cookie.path.clone_from(&default_path);
    }
    cookies.push(cookie);
  }
  cookies
}

#[cfg(test)]
mod tests {
  use super::*;

  fn parse(header: &str, url: &str) -> Option<CookieData> {
    let url = reqwest::Url::parse(url).unwrap();
    let mut headers = reqwest::header::HeaderMap::new();
    headers.append(reqwest::header::SET_COOKIE, header.parse().unwrap());
    parse_set_cookie_headers(&url, &headers).into_iter().next()
  }

  #[test]
  fn host_only_defaults_from_response_url() {
    let c = parse("sid=abc", "http://example.com/a/b/c").unwrap();
    assert_eq!(c.name, "sid");
    assert_eq!(c.value, "abc");
    // Host-only: bare hostname, exact-match semantics.
    assert_eq!(c.domain, "example.com");
    // RFC 6265 default-path: directory of the request path.
    assert_eq!(c.path, "/a/b");
    assert_eq!(c.expires, None);
    assert!(matches!(c.same_site, Some(crate::backend::SameSite::Lax)));
  }

  #[test]
  fn declared_domain_gets_dot_prefixed() {
    let c = parse("sid=1; Domain=example.com; Path=/", "http://example.com/").unwrap();
    assert_eq!(c.domain, ".example.com");
    assert_eq!(c.path, "/");
  }

  #[test]
  fn foreign_domain_is_dropped() {
    assert!(parse("sid=1; Domain=evil.com", "http://example.com/").is_none());
  }

  #[test]
  fn attributes_parse() {
    let c = parse(
      "a=b; Secure; HttpOnly; SameSite=Strict; Max-Age=3600; Path=/x",
      "https://example.com/",
    )
    .unwrap();
    assert!(c.secure);
    assert!(c.http_only);
    assert!(matches!(c.same_site, Some(crate::backend::SameSite::Strict)));
    assert_eq!(c.path, "/x");
    let now = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .unwrap()
      .as_secs_f64();
    let exp = c.expires.unwrap();
    assert!(exp > now + 3500.0 && exp < now + 3700.0);
  }

  #[test]
  fn non_positive_max_age_expires_immediately() {
    let c = parse("a=b; Max-Age=0; Path=/", "http://example.com/").unwrap();
    assert_eq!(c.expires, Some(0.0));
    let c = parse("a=b; Max-Age=-5; Path=/", "http://example.com/").unwrap();
    assert_eq!(c.expires, Some(0.0));
  }

  #[test]
  fn expires_attribute_parses_http_date() {
    let c = parse(
      "a=b; Expires=Wed, 01 Jan 2031 00:00:00 GMT; Path=/",
      "http://example.com/",
    )
    .unwrap();
    let exp = c.expires.unwrap();
    assert!((1_924_991_940.0..1_924_993_000.0).contains(&exp), "got {exp}");
  }

  #[test]
  fn matcher_domain_and_path() {
    let mk = |domain: &str, path: &str, secure: bool| CookieData {
      name: "n".into(),
      value: "v".into(),
      domain: domain.into(),
      path: path.into(),
      secure,
      http_only: false,
      expires: None,
      same_site: None,
      url: None,
    };
    let url = |u: &str| reqwest::Url::parse(u).unwrap();
    // Host-only: exact host, never subdomains.
    assert!(cookie_matches_url(
      &mk("example.com", "/", false),
      &url("http://example.com/")
    ));
    assert!(!cookie_matches_url(
      &mk("example.com", "/", false),
      &url("http://sub.example.com/")
    ));
    // Dot-prefixed: apex + subdomains.
    assert!(cookie_matches_url(
      &mk(".example.com", "/", false),
      &url("http://sub.example.com/")
    ));
    assert!(cookie_matches_url(
      &mk(".example.com", "/", false),
      &url("http://example.com/")
    ));
    // Path prefix on segment boundary.
    assert!(cookie_matches_url(&mk("e.com", "/a", false), &url("http://e.com/a/b")));
    assert!(!cookie_matches_url(&mk("e.com", "/a", false), &url("http://e.com/ab")));
    // Secure only over https, with the localhost carve-out.
    assert!(!cookie_matches_url(&mk("e.com", "/", true), &url("http://e.com/")));
    assert!(cookie_matches_url(&mk("e.com", "/", true), &url("https://e.com/")));
    assert!(cookie_matches_url(
      &mk("localhost", "/", true),
      &url("http://localhost/")
    ));
  }

  #[test]
  fn expired_cookie_is_not_sent() {
    let c = CookieData {
      name: "n".into(),
      value: "v".into(),
      domain: "e.com".into(),
      path: "/".into(),
      secure: false,
      http_only: false,
      expires: Some(1.0),
      same_site: None,
      url: None,
    };
    assert!(!cookie_matches_url(&c, &reqwest::Url::parse("http://e.com/").unwrap()));
    // Backends report session cookies as -1: never expired.
    let session = CookieData {
      expires: Some(-1.0),
      ..c
    };
    assert!(cookie_matches_url(
      &session,
      &reqwest::Url::parse("http://e.com/").unwrap()
    ));
  }
}
