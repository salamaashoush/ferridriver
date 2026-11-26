//! `BrowserContextOptions`, emulation, viewport, network, proxy, and highlight option types.

use super::{EmulateMediaOptions, MediaOverride, RecordVideoOptions, ViewportConfig};

// ── BrowserContextOptions ──────────────────────────────────────────────────

/// Geographic location emulation: `Geolocation { latitude, longitude,
/// accuracy? }`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geolocation {
  /// Latitude between -90 and 90.
  pub latitude: f64,
  /// Longitude between -180 and 180.
  pub longitude: f64,
  /// Non-negative accuracy value. defaults to 0.
  pub accuracy: f64,
}

impl Default for Geolocation {
  fn default() -> Self {
    Self {
      latitude: 0.0,
      longitude: 0.0,
      accuracy: 0.0,
    }
  }
}

/// Network throttling: `{ latency?, downloadThroughput?, uploadThroughput? }`
/// or a preset name. Not in Playwright, which reaches the same CDP command
/// only through `newCDPSession`; Chromium only.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkConditions {
  /// Added round-trip latency, in milliseconds.
  pub latency: f64,
  /// Bytes per second; `None` leaves the direction unthrottled.
  pub download_throughput: Option<f64>,
  pub upload_throughput: Option<f64>,
}

impl NetworkConditions {
  /// No added latency and no bandwidth cap: what clearing the
  /// conditions sends, since the backend has no separate reset.
  pub const NONE: Self = Self {
    latency: 0.0,
    download_throughput: None,
    upload_throughput: None,
  };

  /// The presets Chrome's developer tools offer, with their numbers
  /// (`front_end/core/sdk/NetworkManager.ts`), so a page throttled here
  /// matches what a developer reproduces there.
  pub const PRESETS: &[(&str, Self)] = &[
    (
      "slow-3g",
      Self {
        latency: 2000.0,
        download_throughput: Some(50_000.0),
        upload_throughput: Some(50_000.0),
      },
    ),
    (
      "slow-4g",
      Self {
        latency: 562.5,
        download_throughput: Some(180_000.0),
        upload_throughput: Some(84_375.0),
      },
    ),
    (
      "fast-4g",
      Self {
        latency: 165.0,
        download_throughput: Some(1_012_500.0),
        upload_throughput: Some(168_750.0),
      },
    ),
  ];

  #[must_use]
  pub fn preset(name: &str) -> Option<Self> {
    Self::PRESETS.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
  }

  #[must_use]
  pub fn is_throttled(&self) -> bool {
    *self != Self::NONE
  }

  /// Checks the numbers the way the browser would otherwise reject them
  /// one field at a time, or silently accept a negative latency.
  ///
  /// # Errors
  ///
  /// A negative or non-finite latency or throughput.
  pub fn validate(&self) -> crate::error::Result<()> {
    let fields = [
      ("latency", Some(self.latency)),
      ("downloadThroughput", self.download_throughput),
      ("uploadThroughput", self.upload_throughput),
    ];
    for (name, value) in fields {
      if let Some(v) = value
        && !(v.is_finite() && v >= 0.0)
      {
        return Err(crate::error::FerriError::invalid_argument(
          "networkConditions",
          format!("{name} must be a non-negative number, got {v}"),
        ));
      }
    }
    Ok(())
  }
}

impl<'de> serde::Deserialize<'de> for NetworkConditions {
  fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Fields {
      #[serde(default)]
      latency: f64,
      download_throughput: Option<f64>,
      upload_throughput: Option<f64>,
    }
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Raw {
      Preset(String),
      Fields(Fields),
    }
    let conditions = match Raw::deserialize(deserializer)? {
      Raw::Preset(name) => Self::preset(&name).ok_or_else(|| {
        let known: Vec<&str> = Self::PRESETS.iter().map(|(n, _)| *n).collect();
        serde::de::Error::custom(format!(
          "unknown network preset {name:?}; expected one of {}",
          known.join(", ")
        ))
      })?,
      // CDP spells "unthrottled" as -1, so a caller copying DevTools
      // numbers means the same thing by it.
      Raw::Fields(f) => Self {
        latency: f.latency,
        download_throughput: f.download_throughput.filter(|v| (v + 1.0).abs() > f64::EPSILON),
        upload_throughput: f.upload_throughput.filter(|v| (v + 1.0).abs() > f64::EPSILON),
      },
    };
    conditions.validate().map_err(serde::de::Error::custom)?;
    Ok(conditions)
  }
}

/// HTTP basic/digest credentials: `HTTPCredentials { username, password,
/// origin?, send? }`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HttpCredentials {
  pub username: String,
  pub password: String,
  /// Scheme + host + optional port — restrict credential send to this
  /// origin. `None` sends on any 401 response.
  pub origin: Option<String>,
  /// `"always"` sends credentials on every `APIRequest`; `"unauthorized"`
  /// (default) waits for a 401. default: unauthorized.
  pub send: Option<HttpCredentialsSend>,
}

/// Playwright takes one set of credentials or a list of them
/// (`httpCredentials?: HttpCredentials | HttpCredentials[]`).
impl From<HttpCredentials> for Vec<HttpCredentials> {
  fn from(credentials: HttpCredentials) -> Self {
    vec![credentials]
  }
}

/// The credentials that answer a challenge from `url`: the first entry
/// bound to no origin, or to the origin of `url` whatever its case.
/// Playwright `findMatchingHttpCredentials` (`server/browserContext.ts`).
#[must_use]
pub fn matching_http_credentials<'a>(credentials: &'a [HttpCredentials], url: &str) -> Option<&'a HttpCredentials> {
  let origin = reqwest::Url::parse(url)
    .ok()
    .map(|url| url.origin().ascii_serialization());
  credentials.iter().find(
    |entry| match entry.origin.as_deref().filter(|bound| !bound.is_empty()) {
      None => true,
      Some(bound) => origin
        .as_deref()
        .is_some_and(|origin| bound.eq_ignore_ascii_case(origin)),
    },
  )
}

#[cfg(test)]
mod origin_state_tests {
  use super::{OpfsEntryKind, OriginState};

  /// The shape Playwright writes (`packages/protocol/src/structs.d.ts`).
  const WRITTEN: &str = r#"{
    "origin": "https://example.com",
    "localStorage": [{ "name": "theme", "value": "dark" }],
    "indexedDB": [{
      "name": "library",
      "version": 2,
      "stores": [{
        "name": "books",
        "autoIncrement": false,
        "keyPath": "id",
        "records": [
          { "value": { "id": 1 } },
          { "keyEncoded": { "d": "1970-01-01T00:00:00.000Z" }, "valueEncoded": { "o": [], "id": 1 } }
        ],
        "indexes": [{ "name": "by_name", "keyPathArray": ["a", "b"], "multiEntry": false, "unique": true }]
      }]
    }],
    "opfs": [
      { "path": "docs", "type": "directory" },
      { "path": "docs/note.txt", "type": "file", "base64": "aGk=" }
    ]
  }"#;

  #[test]
  fn an_origin_reads_and_writes_the_shape_playwright_does() {
    let state: OriginState = serde_json::from_str(WRITTEN).unwrap();
    let store = &state.indexed_db.as_ref().unwrap()[0].stores[0];
    assert_eq!(store.key_path.as_deref(), Some("id"));
    assert_eq!(store.records[0].value, Some(serde_json::json!({ "id": 1 })));
    assert!(store.records[1].value.is_none() && store.records[1].value_encoded.is_some());
    assert_eq!(
      store.indexes[0].key_path_array,
      Some(vec!["a".to_string(), "b".to_string()])
    );
    let opfs = state.opfs.as_ref().unwrap();
    assert_eq!(
      (opfs[0].kind, opfs[0].base64.as_deref()),
      (OpfsEntryKind::Directory, None)
    );
    assert_eq!(
      (opfs[1].kind, opfs[1].base64.as_deref()),
      (OpfsEntryKind::File, Some("aGk="))
    );

    let written = serde_json::to_value(&state).unwrap();
    let expected: serde_json::Value = serde_json::from_str(WRITTEN).unwrap();
    assert_eq!(written, expected);
  }

  #[test]
  fn what_was_not_asked_for_is_left_out_of_the_json() {
    let state: OriginState =
      serde_json::from_str(r#"{ "origin": "https://example.com", "localStorage": [] }"#).unwrap();
    assert!(state.indexed_db.is_none() && state.opfs.is_none());
    assert_eq!(
      serde_json::to_value(&state).unwrap(),
      serde_json::json!({ "origin": "https://example.com", "localStorage": [] })
    );
  }

  #[test]
  fn an_origin_is_kept_for_any_kind_of_storage() {
    let with = |json: &str| serde_json::from_str::<OriginState>(json).unwrap().has_storage();
    assert!(!with(r#"{ "origin": "o", "localStorage": [] }"#));
    assert!(!with(
      r#"{ "origin": "o", "localStorage": [], "indexedDB": [], "opfs": [] }"#
    ));
    assert!(with(
      r#"{ "origin": "o", "localStorage": [{ "name": "a", "value": "b" }] }"#
    ));
    assert!(with(
      r#"{ "origin": "o", "localStorage": [], "opfs": [{ "path": "d", "type": "directory" }] }"#
    ));
    assert!(with(
      r#"{ "origin": "o", "localStorage": [], "indexedDB": [{ "name": "db", "version": 1, "stores": [] }] }"#
    ));
  }
}

#[cfg(test)]
mod http_credentials_tests {
  use super::{HttpCredentials, matching_http_credentials};

  fn entry(username: &str, origin: Option<&str>) -> HttpCredentials {
    HttpCredentials {
      username: username.to_string(),
      origin: origin.map(str::to_string),
      ..HttpCredentials::default()
    }
  }

  fn chosen<'a>(list: &'a [HttpCredentials], url: &str) -> Option<&'a str> {
    matching_http_credentials(list, url).map(|entry| entry.username.as_str())
  }

  #[test]
  fn the_first_entry_that_matches_answers() {
    let list = [
      entry("other", Some("https://other.example.com")),
      entry("bound", Some("https://example.com")),
      entry("any", None),
    ];
    assert_eq!(chosen(&list, "https://example.com/a?b#c"), Some("bound"));
    assert_eq!(chosen(&list, "https://other.example.com/"), Some("other"));
    assert_eq!(chosen(&list, "https://third.example.com/"), Some("any"));
    assert_eq!(
      chosen(&[entry("any", None), list[1].clone()], "https://example.com/"),
      Some("any")
    );
  }

  #[test]
  fn an_origin_is_scheme_host_and_port_whatever_the_case() {
    let list = [entry("bound", Some("HTTP://Example.com:8080"))];
    assert_eq!(chosen(&list, "http://example.com:8080/path"), Some("bound"));
    assert_eq!(chosen(&list, "https://example.com:8080/"), None);
    assert_eq!(chosen(&list, "http://example.com/"), None);
    assert_eq!(chosen(&list, "http://www.example.com:8080/"), None);
    // The default port is not part of an origin.
    assert_eq!(
      chosen(
        &[entry("bound", Some("https://example.com"))],
        "https://example.com:443/"
      ),
      Some("bound")
    );
  }

  #[test]
  fn an_entry_bound_to_an_origin_does_not_answer_a_url_without_one() {
    assert_eq!(
      chosen(&[entry("bound", Some("https://example.com"))], "not a url"),
      None
    );
    assert_eq!(chosen(&[entry("any", Some(""))], "not a url"), Some("any"));
    assert_eq!(chosen(&[], "https://example.com/"), None);
  }

  #[test]
  fn one_set_of_credentials_is_a_list_of_one() {
    let list: Vec<HttpCredentials> = entry("only", None).into();
    assert_eq!(list.len(), 1);
  }
}

/// Send policy for [`HttpCredentials`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HttpCredentialsSend {
  /// Only on 401 responses (default).
  #[default]
  Unauthorized,
  /// On every request (`HttpClient` only).
  Always,
}

/// Network proxy configuration.
/// `{ server, bypass?, username?, password? }` — types.d.ts:22412.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProxyConfig {
  pub server: String,
  /// Comma-separated domain list (e.g. `".com, chromium.org"`).
  pub bypass: Option<String>,
  pub username: Option<String>,
  pub password: Option<String>,
}

impl ProxyConfig {
  /// The credentials that answer this proxy's authentication challenge.
  /// Playwright's `proxyCredentials()`: present when a username is.
  #[must_use]
  pub fn credentials(&self) -> Option<HttpCredentials> {
    self
      .username
      .as_deref()
      .filter(|name| !name.is_empty())
      .map(|username| HttpCredentials {
        username: username.to_string(),
        password: self.password.clone().unwrap_or_default(),
        origin: None,
        send: None,
      })
  }

  /// `server` as Chromium takes it. Chromium resolves names on a SOCKS5
  /// proxy whichever way the scheme is spelled but knows only `socks5`, so
  /// `socks5h` is lowered to it.
  #[must_use]
  pub fn chromium_server(&self) -> String {
    let server = self.server_origin();
    match server.strip_prefix("socks5h://") {
      Some(rest) => format!("socks5://{rest}"),
      None => server,
    }
  }

  /// `server` as the origin engines take, `scheme://host[:port]`: `http`
  /// when no scheme is written, without credentials or a path. `WebKit`
  /// finds no host at all in `127.0.0.1:3128`, so the scheme cannot be left
  /// to the engine.
  #[must_use]
  pub fn server_origin(&self) -> String {
    let server = self.server.trim();
    let (scheme, rest) = server.split_once("://").unwrap_or(("http", server));
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    format!("{}://{host}", scheme.to_ascii_lowercase())
  }

  /// `bypass` with each entry trimmed and empty entries dropped. `WebKit`
  /// matches entries verbatim, so `"a.example, localhost"` would otherwise
  /// name a host `" localhost"` that never matches.
  #[must_use]
  pub fn bypass_list(&self) -> Option<String> {
    let entries: Vec<&str> = self
      .bypass
      .as_deref()?
      .split(',')
      .map(str::trim)
      .filter(|entry| !entry.is_empty())
      .collect();
    (!entries.is_empty()).then(|| entries.join(","))
  }

  /// Whether `bypass` names a loopback host, which keeps loopback off the
  /// proxy. Otherwise loopback goes through the proxy like any other host.
  #[must_use]
  pub fn bypasses_loopback(&self) -> bool {
    const LOOPBACK: [&str; 7] = [
      "localhost",
      "127.0.0.1",
      "::1",
      "[::]",
      "[::1]",
      "<loopback>",
      "<-loopback>",
    ];
    self
      .bypass
      .as_deref()
      .is_some_and(|bypass| bypass.split(',').any(|host| LOOPBACK.contains(&host.trim())))
  }

  /// `bypass` as Chromium takes it. Chromium bypasses loopback implicitly
  /// unless the list subtracts it with `<-loopback>`.
  #[must_use]
  pub fn chromium_bypass_list(&self) -> Option<String> {
    let bypass = self.bypass_list();
    if self.bypasses_loopback() {
      return bypass;
    }
    Some(bypass.map_or_else(|| "<-loopback>".to_string(), |bypass| format!("<-loopback>,{bypass}")))
  }

  /// The URL scheme of `server`, `http` when it names none.
  #[must_use]
  pub fn scheme(&self) -> &str {
    self.server.split_once("://").map_or("http", |(scheme, _)| scheme)
  }

  /// Whether this is a SOCKS proxy that wants credentials, which no browser
  /// can send: Chromium, Firefox and `WebKit` speak SOCKS5 without the
  /// username/password method.
  #[must_use]
  pub fn is_authenticated_socks(&self) -> bool {
    self.credentials().is_some() && self.scheme().starts_with("socks")
  }

  /// The proxy as the request client speaks to it.
  #[must_use]
  pub fn transport(&self) -> ferrijs_fetch::Proxy {
    ferrijs_fetch::Proxy {
      server: self.server.clone(),
      bypass: self.bypass.clone(),
      username: self.username.clone(),
      password: self.password.clone(),
    }
  }

  /// Refuse settings no client could use, where they were given rather than
  /// on the first request.
  ///
  /// # Errors
  ///
  /// [`crate::error::FerriError::InvalidArgument`] for a server that is not a
  /// proxy URL, or credentials on a `socks4` proxy.
  pub fn validate(&self) -> crate::error::Result<()> {
    self
      .transport()
      .validate()
      .map_err(|e| crate::error::FerriError::invalid_argument("proxy", e.to_string()))
  }

  /// Command-line flags pointing `backend`'s binary at this proxy.
  ///
  /// The spelling is per engine, exactly as Playwright spells it for the same
  /// binaries: Chromium and the GTK/WPE `MiniBrowser` disagree on the switch
  /// name, and a switch a browser does not know is accepted and ignored — a
  /// proxy that silently does nothing rather than an error.
  ///
  /// Credentials are not a launch flag in any engine; they are answered on the
  /// authentication challenge, so they are not lowered here.
  #[must_use]
  pub fn launch_flags(&self, backend: crate::backend::BackendKind) -> Vec<String> {
    let bypass = self.bypass_list();

    if backend != crate::backend::BackendKind::WebKit {
      let mut flags = vec![format!("--proxy-server={}", self.chromium_server())];
      if let Some(bypass) = self.chromium_bypass_list() {
        flags.push(format!("--proxy-bypass-list={bypass}"));
      }
      return flags;
    }

    let mut flags = vec![format!("--proxy={}", self.server_origin())];
    if let Some(bypass) = bypass {
      if cfg!(target_os = "linux") {
        // The GTK/WPE build takes one host per flag, not a list.
        flags.extend(bypass.split(',').map(|host| format!("--ignore-host={host}")));
      } else {
        flags.push(format!("--proxy-bypass-list={bypass}"));
      }
    }
    flags
  }
}

/// `recordHar` options bag. `recordHar` shape —
/// types.d.ts:22441.
#[derive(Debug, Clone)]
pub struct RecordHarOptions {
  pub path: std::path::PathBuf,
  /// `omit`/`embed`/`attach`. Default derived from `.path` extension.
  pub content: Option<RecordHarContent>,
  /// `full`/`minimal`. Default: full.
  pub mode: Option<RecordHarMode>,
  /// Legacy alias for `content: "omit"`. flags this deprecated
  /// but still accepts it.
  pub omit_content: Option<bool>,
  /// Glob/regex filter for stored requests.
  pub url_filter: Option<crate::url_matcher::UrlMatcher>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordHarContent {
  Omit,
  Embed,
  Attach,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordHarMode {
  Full,
  Minimal,
}

/// Logical viewport dimensions for [`BrowserContextOptions::viewport`].
/// Three states: `Default` (omit → browser default), `Null` (explicit
/// null → opt out of viewport emulation), or `Size(w,h)`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ViewportOption {
  /// Field absent — default 1280x720.
  #[default]
  Default,
  /// Field explicitly `null` — opt out of fixed viewport.
  Null,
  /// Concrete viewport size.
  Size { width: i64, height: i64 },
}

/// `window.screen` size emulation (when viewport is set). Mirrors
/// `screen: { width, height }` — types.d.ts:22539.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenSize {
  pub width: i64,
  pub height: i64,
}

/// A single `localStorage` entry — `{ name, value }`.
/// Mirrors Playwright's `NameValue` (protocol channels.d.ts:5399).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NameValue {
  pub name: String,
  pub value: String,
}

/// Which web-storage area a [`crate::page::Page`] `WebStorage` accessor
/// targets. Mirrors Playwright's `WebStorage` `kind: 'local' | 'session'`
/// (`client/webStorage.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebStorageKind {
  Local,
  Session,
}

/// Per-origin storage snapshot: `{ origin, localStorage, indexedDB?,
/// opfs? }`. Mirrors Playwright's `OriginStorage`
/// (`packages/protocol/src/structs.d.ts`). The last two are present only
/// when `storageState` was asked for them.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OriginState {
  pub origin: String,
  #[serde(default)]
  pub local_storage: Vec<NameValue>,
  #[serde(default, rename = "indexedDB", skip_serializing_if = "Option::is_none")]
  pub indexed_db: Option<Vec<IndexedDbDatabase>>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub opfs: Option<Vec<OpfsEntry>>,
}

impl OriginState {
  /// Whether there is anything here worth keeping in a storage state
  /// (Playwright's `hasStorage`, `server/browserContext.ts`).
  #[must_use]
  pub fn has_storage(&self) -> bool {
    !self.local_storage.is_empty()
      || self.indexed_db.as_ref().is_some_and(|databases| !databases.is_empty())
      || self.opfs.as_ref().is_some_and(|entries| !entries.is_empty())
  }
}

/// One `IndexedDB` database of an origin. Mirrors Playwright's
/// `IndexedDBDatabase` (`packages/protocol/src/structs.d.ts`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedDbDatabase {
  pub name: String,
  pub version: u64,
  pub stores: Vec<IndexedDbStore>,
}

/// One object store of an [`IndexedDbDatabase`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedDbStore {
  pub name: String,
  pub auto_increment: bool,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub key_path: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub key_path_array: Option<Vec<String>>,
  pub records: Vec<IndexedDbRecord>,
  pub indexes: Vec<IndexedDbIndex>,
}

/// One record of an [`IndexedDbStore`]. A key or value that is plain JSON
/// is held as it is; anything richer (a `Date`, a typed array, a cycle)
/// is held in Playwright's evaluate-argument encoding, which only the
/// page-side script reads back.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedDbRecord {
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub key: Option<serde_json::Value>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub key_encoded: Option<serde_json::Value>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub value: Option<serde_json::Value>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub value_encoded: Option<serde_json::Value>,
}

/// One index of an [`IndexedDbStore`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedDbIndex {
  pub name: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub key_path: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub key_path_array: Option<Vec<String>>,
  pub multi_entry: bool,
  pub unique: bool,
}

/// One file or directory of an origin's private file system. Mirrors
/// Playwright's `OPFSEntry` (`packages/protocol/src/structs.d.ts`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OpfsEntry {
  /// Path from the root, `/`-separated.
  pub path: String,
  #[serde(rename = "type")]
  pub kind: OpfsEntryKind,
  /// A file's contents; absent for a directory.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub base64: Option<String>,
}

/// What an [`OpfsEntry`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpfsEntryKind {
  File,
  Directory,
}

/// The state EXPORTED by `context.storageState()` — `{ cookies, origins }`.
/// Mirrors Playwright's `StorageState`
/// (`/tmp/playwright/packages/playwright-core/src/client/types.ts:42`).
/// Serializes to the same JSON shape consumed by [`StorageStateInput`], so a
/// round-trip (export -> `new_context` `storageState`) preserves state.
///
/// `IndexedDB` and the origin private file system are in an origin's entry
/// only when asked for (`storageState({ indexedDB: true, opfs: true })`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StorageState {
  pub cookies: Vec<crate::backend::CookieData>,
  pub origins: Vec<OriginState>,
  /// The context's `WebAuthn` passkeys, present only when asked for with
  /// `storageState({ credentials: true })`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub credentials: Option<Vec<crate::credentials::VirtualCredential>>,
}

/// Options for `context.storageState(options?)`.
/// `{ path?: string, indexedDB?: boolean, opfs?: boolean, credentials?: boolean }`
/// (`packages/playwright-core/src/client/browserContext.ts`).
#[derive(Debug, Clone, Default)]
pub struct StorageStateOptions {
  /// Write the JSON-serialized state to this file (pretty-printed, 2-space).
  pub path: Option<std::path::PathBuf>,
  /// Collect each origin's `IndexedDB` databases as well.
  pub indexed_db: Option<bool>,
  /// Collect each origin's private file system as well. An engine that
  /// has none for the context fails the call rather than leaving it out.
  pub opfs: Option<bool>,
  /// Include the context's `WebAuthn` credentials.
  pub credentials: Option<bool>,
}

/// Storage state bag — cookies + per-origin localStorage snapshot.
/// `storageState: string | { cookies, origins }` —
/// types.d.ts:22566.
#[derive(Debug, Clone)]
pub enum StorageStateInput {
  /// Path to a JSON file written by `context.storageState({ path })`.
  Path(std::path::PathBuf),
  /// Inline state object.
  Inline(serde_json::Value),
}

/// Service-worker policy.
/// `serviceWorkers: "allow" | "block"` — types.d.ts:22557.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ServiceWorkerPolicy {
  #[default]
  Allow,
  Block,
}

/// `BrowserContextOptions` — the option bag accepted by
/// `Browser::new_context`. Full 28-field shape.
///
/// Every field is optional. `None` means "browser default"; an explicit
/// value applies the corresponding emulation at every page opened in
/// the context. Several fields have sub-options that distinguish
/// explicit `null` from absent (e.g. `viewport: null` to disable
/// viewport emulation vs. omitted).
///
/// Construction: use [`BrowserContextOptions::default`] and set fields
/// field-by-field, or use any of the dedicated builder helpers defined
/// inline.
#[derive(Debug, Clone, Default)]
pub struct BrowserContextOptions {
  pub accept_downloads: Option<bool>,
  /// Attribute `getByTestId` reads in this context. Playwright carries
  /// it as `testIdAttributeName` on the context options; `None` leaves
  /// the process default (`selectors.setTestIdAttribute`, else
  /// `data-testid`).
  pub test_id_attribute: Option<String>,
  pub base_url: Option<String>,
  pub bypass_csp: Option<bool>,
  /// `null` → no emulation, the page follows the browser's own
  /// appearance; a value → apply it; absent → `light`, whatever the
  /// host's appearance, as in Playwright.
  pub color_scheme: MediaOverride,
  pub contrast: MediaOverride,
  pub device_scale_factor: Option<f64>,
  pub extra_http_headers: Option<rustc_hash::FxHashMap<String, String>>,
  pub forced_colors: MediaOverride,
  pub geolocation: Option<Geolocation>,
  pub has_touch: Option<bool>,
  pub http_credentials: Option<Vec<HttpCredentials>>,
  pub ignore_https_errors: Option<bool>,
  pub is_mobile: Option<bool>,
  pub java_script_enabled: Option<bool>,
  pub locale: Option<String>,
  pub offline: Option<bool>,
  /// Clearing stores [`NetworkConditions::NONE`] rather than `None`, so the
  /// reset reaches pages that were throttled.
  pub network_conditions: Option<NetworkConditions>,
  /// Chromium's CPU slowdown factor; `1.0` is unthrottled and is what
  /// clearing stores, for the same reason.
  pub cpu_throttling_rate: Option<f64>,
  pub permissions: Option<Vec<String>>,
  /// Grants made with `browserContext.grantPermissions(permissions, { origin })`,
  /// by origin; `permissions` above holds the ones for every origin.
  pub origin_permissions: Vec<(String, Vec<String>)>,
  pub proxy: Option<ProxyConfig>,
  pub record_har: Option<RecordHarOptions>,
  pub record_video: Option<RecordVideoOptions>,
  pub reduced_motion: MediaOverride,
  pub screen: Option<ScreenSize>,
  pub service_workers: Option<ServiceWorkerPolicy>,
  pub storage_state: Option<StorageStateInput>,
  pub strict_selectors: Option<bool>,
  pub timezone_id: Option<String>,
  pub user_agent: Option<String>,
  pub viewport: ViewportOption,
}

impl BrowserContextOptions {
  /// Resolve [`Self::viewport`] to the [`ViewportConfig`] a freshly
  /// opened page should be emulated with. Returns `None` when the
  /// caller passed `viewport: null` — the page inherits the backend's
  /// native window size. `ViewportOption::Default` folds in
  /// `device_scale_factor`, `is_mobile`, and `has_touch` into the
  /// default 1280x720; explicit `Size(w,h)` likewise.
  #[must_use]
  pub fn resolved_viewport(&self) -> Option<ViewportConfig> {
    let (width, height) = match self.viewport {
      ViewportOption::Null => return None,
      ViewportOption::Default => (
        crate::state::DEFAULT_VIEWPORT_WIDTH,
        crate::state::DEFAULT_VIEWPORT_HEIGHT,
      ),
      ViewportOption::Size { width, height } => (width, height),
    };
    Some(ViewportConfig {
      width,
      height,
      device_scale_factor: self.device_scale_factor.unwrap_or(1.0),
      is_mobile: self.is_mobile.unwrap_or(false),
      has_touch: self.has_touch.unwrap_or(false),
      is_landscape: false,
    })
  }

  /// The options `self` names, falling back to `base` for each one it
  /// leaves out.
  ///
  /// Playwright's test runner fills a `browser.newContext(options)` made
  /// inside a test from the test's own `use` options this way, key by
  /// key, only where the caller named nothing
  /// (`runBeforeCreateBrowserContext`, `playwright/src/index.ts`).
  #[must_use]
  pub fn layered_over(&self, base: &Self) -> Self {
    let media = |own: &MediaOverride, base: &MediaOverride| {
      if own.is_specified() { own.clone() } else { base.clone() }
    };
    Self {
      accept_downloads: self.accept_downloads.or(base.accept_downloads),
      test_id_attribute: self
        .test_id_attribute
        .clone()
        .or_else(|| base.test_id_attribute.clone()),
      base_url: self.base_url.clone().or_else(|| base.base_url.clone()),
      bypass_csp: self.bypass_csp.or(base.bypass_csp),
      color_scheme: media(&self.color_scheme, &base.color_scheme),
      contrast: media(&self.contrast, &base.contrast),
      device_scale_factor: self.device_scale_factor.or(base.device_scale_factor),
      extra_http_headers: self
        .extra_http_headers
        .clone()
        .or_else(|| base.extra_http_headers.clone()),
      forced_colors: media(&self.forced_colors, &base.forced_colors),
      geolocation: self.geolocation.or(base.geolocation),
      has_touch: self.has_touch.or(base.has_touch),
      http_credentials: self.http_credentials.clone().or_else(|| base.http_credentials.clone()),
      ignore_https_errors: self.ignore_https_errors.or(base.ignore_https_errors),
      is_mobile: self.is_mobile.or(base.is_mobile),
      java_script_enabled: self.java_script_enabled.or(base.java_script_enabled),
      locale: self.locale.clone().or_else(|| base.locale.clone()),
      offline: self.offline.or(base.offline),
      network_conditions: self.network_conditions.or(base.network_conditions),
      cpu_throttling_rate: self.cpu_throttling_rate.or(base.cpu_throttling_rate),
      permissions: self.permissions.clone().or_else(|| base.permissions.clone()),
      origin_permissions: if self.origin_permissions.is_empty() {
        base.origin_permissions.clone()
      } else {
        self.origin_permissions.clone()
      },
      proxy: self.proxy.clone().or_else(|| base.proxy.clone()),
      record_har: self.record_har.clone().or_else(|| base.record_har.clone()),
      record_video: self.record_video.clone().or_else(|| base.record_video.clone()),
      reduced_motion: media(&self.reduced_motion, &base.reduced_motion),
      screen: self.screen.or(base.screen),
      service_workers: self.service_workers.or(base.service_workers),
      storage_state: self.storage_state.clone().or_else(|| base.storage_state.clone()),
      strict_selectors: self.strict_selectors.or(base.strict_selectors),
      timezone_id: self.timezone_id.clone().or_else(|| base.timezone_id.clone()),
      user_agent: self.user_agent.clone().or_else(|| base.user_agent.clone()),
      viewport: match self.viewport {
        ViewportOption::Default => base.viewport.clone(),
        ref own => own.clone(),
      },
    }
  }

  /// The media features this context names for its pages, which a
  /// page's own `emulateMedia` calls are layered over.
  #[must_use]
  pub fn emulated_media(&self) -> EmulateMediaOptions {
    EmulateMediaOptions {
      media: MediaOverride::Unchanged,
      color_scheme: self.color_scheme.clone(),
      reduced_motion: self.reduced_motion.clone(),
      forced_colors: self.forced_colors.clone(),
      contrast: self.contrast.clone(),
    }
  }
}

/// Selector for [`crate::Page::frame`]. The `page.frame(frameSelector)`
/// union type `string | { name?: string; url?:
/// string|RegExp|URLPattern|(url => bool) }`.
///
/// Today we accept the string form + `{ name, url }` with both fields
/// being plain strings (exact match). Future work extends `url` to the
/// full `StringOrRegex` matcher; matching rules will remain behind this
/// struct so callers don't rebind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameSelector {
  /// Match against the frame's `name` attribute (exact).
  pub name: Option<String>,
  /// Match against the frame's URL (exact).
  pub url: Option<String>,
}

impl FrameSelector {
  /// Convenience: selector that matches by frame name.
  #[must_use]
  pub fn by_name(name: impl Into<String>) -> Self {
    Self {
      name: Some(name.into()),
      url: None,
    }
  }

  /// Convenience: selector that matches by frame URL.
  #[must_use]
  pub fn by_url(url: impl Into<String>) -> Self {
    Self {
      name: None,
      url: Some(url.into()),
    }
  }

  /// Returns `true` when neither `name` nor `url` is set.
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.name.is_none() && self.url.is_none()
  }
}

impl From<&str> for FrameSelector {
  fn from(name: &str) -> Self {
    Self::by_name(name)
  }
}

impl From<String> for FrameSelector {
  fn from(name: String) -> Self {
    Self::by_name(name)
  }
}

impl From<&String> for FrameSelector {
  fn from(name: &String) -> Self {
    Self::by_name(name.clone())
  }
}

/// The `style` argument of `Locator.highlight`. Playwright:
/// `highlight(options: { style?: string | Record<string, string | number> })`
/// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:158`).
/// A `Css` string is forwarded verbatim; an `Object` map is collapsed to
/// a CSS declaration string the same way Playwright's `cssObjectToString`
/// does (`camelCase` -> `kebab-case`, `--custom-props` preserved,
/// `"k: v"` joined with `"; "`).
#[derive(Debug, Clone)]
pub enum HighlightStyle {
  Css(String),
  Object(Vec<(String, String)>),
}

impl HighlightStyle {
  /// Resolve to the CSS string the injected highlight overlay applies.
  /// Mirrors Playwright's `cssObjectToString`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:486`).
  #[must_use]
  pub fn to_css_string(&self) -> String {
    match self {
      Self::Css(s) => s.clone(),
      Self::Object(entries) => entries
        .iter()
        .map(|(key, value)| {
          let property = if key.starts_with("--") {
            key.clone()
          } else {
            let mut out = String::with_capacity(key.len() + 4);
            for ch in key.chars() {
              if ch.is_ascii_uppercase() {
                out.push('-');
                out.push(ch.to_ascii_lowercase());
              } else {
                out.push(ch);
              }
            }
            out
          };
          format!("{property}: {value}")
        })
        .collect::<Vec<_>>()
        .join("; "),
    }
  }
}

impl From<&str> for HighlightStyle {
  fn from(s: &str) -> Self {
    Self::Css(s.to_string())
  }
}

impl From<String> for HighlightStyle {
  fn from(s: String) -> Self {
    Self::Css(s)
  }
}
