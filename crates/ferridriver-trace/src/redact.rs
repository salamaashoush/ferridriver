//! Removing secret values from a finished trace.
//!
//! A trace records what the script typed (`fill` params), what the page
//! showed (DOM snapshots, input values included), what it logged, and every
//! request with its headers and bodies. Any of those can carry a secret the
//! script was given, so a trace that leaves the machine is rewritten first:
//! every known value, and the forms it travels in (percent-encoded in a URL,
//! base64 in a basic-auth header), is replaced by a marker naming the secret,
//! and password inputs are blanked whatever they held.
//!
//! This is an exact-match pass over text. It cannot find a secret the page
//! transformed (hashed, split, re-encoded some other way), and it cannot
//! touch pixels: a secret typed into a visible text field still shows in
//! screenshots, the film strip and video.

use std::borrow::Cow;
use std::io::{Read as _, Write as _};
use std::path::Path;

use base64::Engine as _;
use serde_json::Value;

/// Values shorter than this are not redacted: replacing every `1` or `ab`
/// in a trace would destroy it and hide nothing.
pub const MIN_SECRET_LEN: usize = 4;

/// The marker `ferridriver run` and `ferridriver test` write in place of a
/// secret.
#[must_use]
pub fn secret_marker(name: &str) -> String {
  format!("<secret>{name}</secret>")
}

/// Replaces known secret values in text and JSON.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
  /// `(needle, replacement)`, longest needle first so a value that contains
  /// another is replaced whole.
  needles: Vec<(String, String)>,
  /// `(name, value)` as declared, short values included.
  declared: Vec<(String, String)>,
}

impl Redactor {
  /// A redactor for `secrets` (`(name, value)` pairs) that writes
  /// [`secret_marker`] in their place.
  #[must_use]
  pub fn new(secrets: impl IntoIterator<Item = (String, String)>) -> Self {
    Self::with_marker(secrets, secret_marker)
  }

  /// As [`Self::new`], with the replacement text chosen by `marker`.
  #[must_use]
  pub fn with_marker(secrets: impl IntoIterator<Item = (String, String)>, marker: impl Fn(&str) -> String) -> Self {
    let declared: Vec<(String, String)> = secrets.into_iter().filter(|(_, value)| !value.is_empty()).collect();
    let mut needles = Vec::new();
    for (name, value) in &declared {
      if value.len() < MIN_SECRET_LEN {
        continue;
      }
      let replacement = marker(name);
      for form in forms(value) {
        if !needles.iter().any(|(needle, _)| needle == &form) {
          needles.push((form, replacement.clone()));
        }
      }
    }
    needles.sort_by_key(|(needle, _)| std::cmp::Reverse(needle.len()));
    Self { needles, declared }
  }

  /// Whether no secret was declared at all; empty values are not secrets.
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.declared.is_empty()
  }

  /// The declared name of an exactly-matching secret value.
  #[must_use]
  pub fn name_for(&self, value: &str) -> Option<&str> {
    self
      .declared
      .iter()
      .find(|(_, secret)| secret == value)
      .map(|(name, _)| name.as_str())
  }

  #[must_use]
  pub fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
    if !self.needles.iter().any(|(needle, _)| text.contains(needle.as_str())) {
      return Cow::Borrowed(text);
    }
    let mut out = text.to_owned();
    for (needle, replacement) in &self.needles {
      if out.contains(needle.as_str()) {
        out = out.replace(needle.as_str(), replacement);
      }
    }
    Cow::Owned(out)
  }

  /// Bytes that may or may not be text, every spelling of a secret replaced
  /// wherever its bytes appear: a body that is not UTF-8 (another charset,
  /// or a binary format carrying text) holds a secret's bytes just the same.
  #[must_use]
  pub fn redact_bytes<'a>(&self, bytes: &'a [u8]) -> Cow<'a, [u8]> {
    if let Ok(text) = std::str::from_utf8(bytes) {
      return match self.redact(text) {
        Cow::Borrowed(_) => Cow::Borrowed(bytes),
        Cow::Owned(text) => Cow::Owned(text.into_bytes()),
      };
    }
    let mut out: Cow<'a, [u8]> = Cow::Borrowed(bytes);
    for (needle, replacement) in &self.needles {
      if let Some(replaced) = replace_bytes(&out, needle.as_bytes(), replacement.as_bytes()) {
        out = Cow::Owned(replaced);
      }
    }
    out
  }

  /// Every string and object key in `value`, in place.
  pub fn redact_json(&self, value: &mut Value) {
    if self.is_empty() {
      return;
    }
    match value {
      Value::String(text) => {
        if let Cow::Owned(redacted) = self.redact(text) {
          *text = redacted;
        }
      },
      Value::Array(items) => items.iter_mut().for_each(|item| self.redact_json(item)),
      Value::Object(map) => {
        let keys: Vec<String> = map.keys().cloned().collect();
        for key in keys {
          let Some(mut child) = map.remove(&key) else { continue };
          self.redact_json(&mut child);
          map.insert(self.redact(&key).into_owned(), child);
        }
      },
      _ => {},
    }
  }
}

/// Bytes left as they are by `encodeURIComponent`, by a form body
/// (`application/x-www-form-urlencoded`, `URLSearchParams`), and by RFC 3986
/// percent-encoding of everything but the unreserved set.
const KEPT: [&[u8]; 3] = [b"-_.!~*'()", b"-_.*", b"-_.~"];

/// The spellings a value travels in: as is; percent-encoded the ways URLs
/// and form bodies carry it, with either case of hex digit and a space as
/// `%20` or `+`; escaped as a JSON string; and base64 in both alphabets
/// (basic auth, data URLs, tokens).
fn forms(value: &str) -> Vec<String> {
  let mut forms = vec![value.to_owned()];
  for kept in KEPT {
    for upper in [true, false] {
      let encoded = percent_encode(value, kept, upper);
      forms.push(encoded.replace("%20", "+"));
      forms.push(encoded);
    }
  }
  if let Ok(quoted) = serde_json::to_string(value) {
    let escaped = &quoted[1..quoted.len() - 1];
    forms.push(escaped.to_owned());
    forms.push(escaped.replace('/', "\\/"));
    let ascii = ascii_json(escaped);
    forms.push(ascii.replace('/', "\\/"));
    forms.push(ascii);
  }
  for engine in [
    &base64::engine::general_purpose::STANDARD,
    &base64::engine::general_purpose::STANDARD_NO_PAD,
    &base64::engine::general_purpose::URL_SAFE,
    &base64::engine::general_purpose::URL_SAFE_NO_PAD,
  ] {
    forms.push(engine.encode(value));
  }
  let mut seen = std::collections::HashSet::new();
  forms.retain(|form| seen.insert(form.clone()));
  forms
}

fn percent_encode(value: &str, kept: &[u8], upper: bool) -> String {
  use std::fmt::Write as _;
  let mut out = String::with_capacity(value.len());
  for byte in value.bytes() {
    if byte.is_ascii_alphanumeric() || kept.contains(&byte) {
      out.push(char::from(byte));
    } else if upper {
      let _ = write!(out, "%{byte:02X}");
    } else {
      let _ = write!(out, "%{byte:02x}");
    }
  }
  out
}

/// A JSON string body with every character beyond ASCII written as a
/// `\u` escape, as serializers that keep their output ASCII write it.
fn ascii_json(escaped: &str) -> String {
  use std::fmt::Write as _;
  let mut out = String::with_capacity(escaped.len());
  for c in escaped.chars() {
    if c.is_ascii() {
      out.push(c);
    } else {
      let mut units = [0_u16; 2];
      for unit in c.encode_utf16(&mut units) {
        let _ = write!(out, "\\u{unit:04x}");
      }
    }
  }
  out
}

/// `haystack` with every `needle` replaced, or `None` when it holds none.
fn replace_bytes(haystack: &[u8], needle: &[u8], replacement: &[u8]) -> Option<Vec<u8>> {
  if needle.is_empty() || !haystack.windows(needle.len()).any(|window| window == needle) {
    return None;
  }
  let mut out = Vec::with_capacity(haystack.len());
  let mut at = 0;
  while at < haystack.len() {
    if haystack[at..].starts_with(needle) {
      out.extend_from_slice(replacement);
      at += needle.len();
    } else {
      out.push(haystack[at]);
      at += 1;
    }
  }
  Some(out)
}

/// Request and response headers that carry a credential whatever the
/// secrets are: a session cookie or a basic-auth pair is never a declared
/// value, and is the account itself to whoever reads it.
const CREDENTIAL_HEADERS: [&str; 4] = ["authorization", "proxy-authorization", "cookie", "set-cookie"];

/// What a blanked credential reads as.
pub const BLANKED: &str = "<credential>";

/// Blanks credential headers and cookie values in a HAR-shaped request or
/// response (`headers: [{name, value}]`, `cookies: [{name, value}]`).
fn blank_credentials(message: &mut Value) {
  if let Some(headers) = message.get_mut("headers").and_then(Value::as_array_mut) {
    for header in headers {
      let credential = header
        .get("name")
        .and_then(Value::as_str)
        .is_some_and(|name| CREDENTIAL_HEADERS.iter().any(|known| name.eq_ignore_ascii_case(known)));
      if credential && let Some(value) = header.get_mut("value") {
        *value = Value::String(BLANKED.to_owned());
      }
    }
  }
  if let Some(cookies) = message.get_mut("cookies").and_then(Value::as_array_mut) {
    for cookie in cookies {
      if let Some(value) = cookie.get_mut("value") {
        *value = Value::String(BLANKED.to_owned());
      }
    }
  }
}

/// A HAR log with every secret removed: credential headers and cookies
/// blanked, base64 bodies decoded, redacted and encoded again, and every
/// other string redacted as text.
pub fn redact_har(har: &mut Value, redactor: &Redactor) {
  if let Some(entries) = har.pointer_mut("/log/entries").and_then(Value::as_array_mut) {
    for entry in entries {
      for side in ["request", "response"] {
        if let Some(message) = entry.get_mut(side) {
          blank_credentials(message);
        }
      }
      if let Some(content) = entry.pointer_mut("/response/content")
        && content.get("encoding").and_then(Value::as_str) == Some("base64")
        && let Some(text) = content.get("text").and_then(Value::as_str)
        && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(text)
        && let Cow::Owned(redacted) = redactor.redact_bytes(&bytes)
      {
        content["text"] = Value::String(base64::engine::general_purpose::STANDARD.encode(redacted));
      }
    }
  }
  redactor.redact_json(har);
}

/// What [`rewrite_zip`] does besides redacting.
#[derive(Debug, Clone, Copy, Default)]
pub struct RewriteOptions {
  /// Drop response bodies of images, fonts, audio and video: they are most
  /// of a trace's size and rarely what explains a failure.
  pub drop_media_bodies: bool,
  /// Drop image attachments (the failure screenshot the runner also writes
  /// beside the trace).
  pub drop_image_attachments: bool,
  /// Blank credential headers and cookies in every recorded request and
  /// response, for a trace that leaves the machine.
  pub blank_credentials: bool,
}

/// What [`rewrite_zip`] changed.
#[derive(Debug, Clone, Default)]
pub struct RewriteReport {
  pub entries: usize,
  pub dropped: Vec<String>,
}

/// Write `src` to `dst` with secrets redacted, password inputs blanked, and
/// whatever `options` drops left out. `src` and `dst` may be the same path.
///
/// # Errors
///
/// Errors when the archive cannot be read or the new one cannot be written.
pub fn rewrite_zip(
  src: &Path,
  dst: &Path,
  redactor: &Redactor,
  options: RewriteOptions,
) -> Result<RewriteReport, String> {
  let err = |e: &dyn std::fmt::Display| format!("{}: {e}", src.display());
  let mut archive = zip::ZipArchive::new(std::fs::File::open(src).map_err(|e| err(&e))?).map_err(|e| err(&e))?;

  // The streams decide which resources are dropped, so they are read first.
  let mut streams: Vec<(String, Vec<u8>)> = Vec::new();
  let mut dropped = std::collections::BTreeSet::new();
  for index in 0..archive.len() {
    let mut entry = archive.by_index(index).map_err(|e| err(&e))?;
    let name = entry.name().map_err(|e| err(&e))?.into_owned();
    let stream = Path::new(&name)
      .extension()
      .is_some_and(|ext| ext.eq_ignore_ascii_case("trace") || ext.eq_ignore_ascii_case("network"));
    if !stream {
      continue;
    }
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).map_err(|e| err(&e))?;
    let rewritten = rewrite_stream(&bytes, redactor, options, &mut dropped);
    streams.push((name, rewritten));
  }

  // A fresh name, created exclusively: a fixed one could already be a link
  // to somewhere else.
  let parent = dst
    .parent()
    .filter(|parent| !parent.as_os_str().is_empty())
    .unwrap_or(Path::new("."));
  let (file, tmp) = exclusive_temp(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
  let mut writer = zip::ZipWriter::new(file);
  let mut report = RewriteReport::default();
  for index in 0..archive.len() {
    let mut entry = archive.by_index(index).map_err(|e| err(&e))?;
    let name = entry.name().map_err(|e| err(&e))?.into_owned();
    if dropped.contains(&name) {
      report.dropped.push(name);
      continue;
    }
    let method = entry.compression();
    let options = zip::write::SimpleFileOptions::default().compression_method(method);
    if let Some((_, bytes)) = streams.iter().find(|(stream, _)| *stream == name) {
      writer.start_file(name.as_str(), options).map_err(|e| err(&e))?;
      writer.write_all(bytes).map_err(|e| err(&e))?;
    } else {
      let mut bytes = Vec::new();
      entry.read_to_end(&mut bytes).map_err(|e| err(&e))?;
      drop(entry);
      if let Cow::Owned(redacted) = redactor.redact_bytes(&bytes) {
        writer.start_file(name.as_str(), options).map_err(|e| err(&e))?;
        writer.write_all(&redacted).map_err(|e| err(&e))?;
      } else {
        let entry = archive.by_index_raw(index).map_err(|e| err(&e))?;
        writer.raw_copy_file(entry).map_err(|e| err(&e))?;
      }
    }
    report.entries += 1;
  }
  writer.finish().map_err(|e| err(&e))?;
  std::fs::rename(&tmp, dst).map_err(|e| {
    let _ = std::fs::remove_file(&tmp);
    format!("{}: {e}", dst.display())
  })?;
  Ok(report)
}

fn exclusive_temp(dir: &Path) -> std::io::Result<(std::fs::File, std::path::PathBuf)> {
  use std::hash::{BuildHasher as _, Hasher as _};
  let mut attempt = 0_u32;
  loop {
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u32(attempt);
    hasher.write_u128(
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos()),
    );
    let path = dir.join(format!(".rewrite-{:016x}.tmp", hasher.finish()));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
      Ok(file) => return Ok((file, path)),
      Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && attempt < 16 => attempt += 1,
      Err(error) => return Err(error),
    }
  }
}

const MEDIA: &[&str] = &[
  "image/",
  "font/",
  "audio/",
  "video/",
  "application/font",
  "application/x-font",
];

fn rewrite_stream(
  bytes: &[u8],
  redactor: &Redactor,
  options: RewriteOptions,
  dropped: &mut std::collections::BTreeSet<String>,
) -> Vec<u8> {
  let text = String::from_utf8_lossy(bytes);
  let mut out = String::with_capacity(text.len());
  for line in text.lines() {
    let Ok(mut event) = serde_json::from_str::<Value>(line) else {
      out.push_str(&redactor.redact(line));
      out.push('\n');
      continue;
    };
    if options.blank_credentials && event.get("type").and_then(Value::as_str) == Some("resource-snapshot") {
      for side in ["request", "response"] {
        if let Some(message) = event.get_mut("snapshot").and_then(|snapshot| snapshot.get_mut(side)) {
          blank_credentials(message);
        }
      }
    }
    match event.get("type").and_then(Value::as_str) {
      Some("frame-snapshot") => {
        if let Some(html) = event.get_mut("snapshot").and_then(|s| s.get_mut("html")) {
          blank_passwords(html);
        }
      },
      Some("resource-snapshot") if options.drop_media_bodies => {
        if let Some(content) = event.pointer_mut("/snapshot/response/content")
          && content
            .get("mimeType")
            .and_then(Value::as_str)
            .is_some_and(|mime| MEDIA.iter().any(|prefix| mime.starts_with(prefix)))
          && let Some(sha1) = content.as_object_mut().and_then(|c| c.remove("_sha1"))
          && let Some(sha1) = sha1.as_str()
        {
          dropped.insert(format!("resources/{sha1}"));
        }
      },
      Some("after") if options.drop_image_attachments => {
        if let Some(attachments) = event.get_mut("attachments").and_then(Value::as_array_mut) {
          attachments.retain(|attachment| {
            let image = attachment
              .get("contentType")
              .and_then(Value::as_str)
              .is_some_and(|ct| ct.starts_with("image/"));
            if image && let Some(sha1) = attachment.get("sha1").and_then(Value::as_str) {
              dropped.insert(format!("resources/{sha1}"));
            }
            !image
          });
        }
      },
      _ => {},
    }
    redactor.redact_json(&mut event);
    out.push_str(&event.to_string());
    out.push('\n');
  }
  out.into_bytes()
}

/// Blank the value of every `<input type=password>` in a DOM snapshot.
/// Snapshot nodes are `[tag, attrs, ...children]`, text, or back-references.
fn blank_passwords(node: &mut Value) {
  let Value::Array(items) = node else { return };
  let is_password_input = items
    .first()
    .and_then(Value::as_str)
    .is_some_and(|tag| tag.eq_ignore_ascii_case("input"))
    && items
      .get(1)
      .and_then(|attrs| attrs.get("type"))
      .and_then(Value::as_str)
      .is_some_and(|kind| kind.eq_ignore_ascii_case("password"));
  if is_password_input && let Some(Value::Object(attrs)) = items.get_mut(1) {
    for key in ["value", "__playwright_value_"] {
      if attrs.contains_key(key) {
        attrs.insert(key.to_owned(), Value::String(String::new()));
      }
    }
  }
  for child in items.iter_mut() {
    blank_passwords(child);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const SECRET: &str = "Zq7 s3cr3t/value";

  fn redactor() -> Redactor {
    Redactor::new([
      ("APP_PASSWORD".to_owned(), SECRET.to_owned()),
      ("PIN".to_owned(), "12".to_owned()),
    ])
  }

  #[test]
  fn replaces_every_form_of_a_value() {
    let r = redactor();
    let encoded = percent_encode(SECRET, KEPT[0], true);
    let b64 = base64::engine::general_purpose::STANDARD.encode(SECRET);
    let text = format!("typed {SECRET}, sent ?p={encoded}, auth Basic {b64}");
    assert_eq!(
      r.redact(&text),
      "typed <secret>APP_PASSWORD</secret>, sent ?p=<secret>APP_PASSWORD</secret>, auth Basic <secret>APP_PASSWORD</secret>"
    );
  }

  #[test]
  fn short_values_are_left_alone() {
    assert_eq!(redactor().redact("PIN 12"), "PIN 12");
  }

  #[test]
  fn walks_json_strings_and_keys() {
    let mut value = serde_json::json!({ "value": SECRET, SECRET: [1, { "x": format!("a {SECRET} b") }] });
    redactor().redact_json(&mut value);
    let text = value.to_string();
    assert!(!text.contains(SECRET), "{text}");
    assert!(text.contains("<secret>APP_PASSWORD</secret>"));
  }

  fn write_zip(path: &Path, entries: &[(&str, &[u8], zip::CompressionMethod)]) {
    let mut writer = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for (name, bytes, method) in entries {
      writer
        .start_file(
          *name,
          zip::write::SimpleFileOptions::default().compression_method(*method),
        )
        .unwrap();
      writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap();
  }

  fn read_all(path: &Path) -> Vec<(String, Vec<u8>)> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    (0..archive.len())
      .map(|i| {
        let mut entry = archive.by_index(i).unwrap();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        (entry.name().unwrap().into_owned(), bytes)
      })
      .collect()
  }

  #[test]
  fn a_rewritten_trace_holds_no_secret_anywhere() {
    let encoded = percent_encode(SECRET, KEPT[0], true);
    let trace = [
      serde_json::json!({ "type": "before", "callId": "call@1", "class": "Locator", "method": "fill",
        "params": { "selector": "#pw", "value": SECRET } }),
      serde_json::json!({ "type": "log", "callId": "call@1", "message": format!("filling {SECRET}") }),
      serde_json::json!({ "type": "console", "text": format!("token is {SECRET}") }),
      serde_json::json!({ "type": "frame-snapshot", "snapshot": { "html": ["HTML", {}, ["BODY", {},
        ["INPUT", { "type": "password", "__playwright_value_": "hunter2-unknown" }],
        ["INPUT", { "type": "text", "__playwright_value_": SECRET }],
        ["P", {}, format!("shown {SECRET}")]]] } }),
      serde_json::json!({ "type": "after", "callId": "call@2", "attachments": [
        { "name": "screenshot-on-failure", "contentType": "image/png", "sha1": "shot.png" }] }),
    ]
    .map(|event| event.to_string())
    .join("\n");
    let network = [
      serde_json::json!({ "type": "resource-snapshot", "snapshot": {
        "request": { "url": format!("https://a.test/?p={encoded}"), "headers": [{ "name": "x-token", "value": SECRET },
            { "name": "Cookie", "value": "sid=session-cookie-value" }],
          "postData": { "text": format!("password={encoded}") } },
        "response": { "status": 200, "content": { "mimeType": "text/html", "_sha1": "page.html" } } } }),
      serde_json::json!({ "type": "resource-snapshot", "snapshot": {
        "request": { "url": "https://a.test/logo.png", "headers": [] },
        "response": { "status": 200, "content": { "mimeType": "image/png", "_sha1": "logo.png" } } } }),
    ]
    .map(|event| event.to_string())
    .join("\n");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace.zip");
    let html = format!("<p>{SECRET}</p>");
    write_zip(
      &path,
      &[
        ("trace.trace", trace.as_bytes(), zip::CompressionMethod::Deflated),
        ("trace.network", network.as_bytes(), zip::CompressionMethod::Deflated),
        ("resources/page.html", html.as_bytes(), zip::CompressionMethod::Deflated),
        ("resources/logo.png", b"\x89PNG", zip::CompressionMethod::Stored),
        ("resources/shot.png", b"\x89PNG", zip::CompressionMethod::Stored),
        ("resources/page@1-1.jpeg", b"\xff\xd8", zip::CompressionMethod::Stored),
        (
          "resources/body.bin",
          &[&[0xff_u8, 0xfe][..], SECRET.as_bytes()].concat(),
          zip::CompressionMethod::Stored,
        ),
      ],
    );
    let report = rewrite_zip(
      &path,
      &path,
      &redactor(),
      RewriteOptions {
        drop_media_bodies: true,
        drop_image_attachments: true,
        blank_credentials: true,
      },
    )
    .unwrap();

    let entries = read_all(&path);
    let names: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
      names,
      [
        "trace.trace",
        "trace.network",
        "resources/page.html",
        "resources/page@1-1.jpeg",
        "resources/body.bin"
      ]
    );
    assert_eq!(report.dropped, ["resources/logo.png", "resources/shot.png"]);
    for (name, bytes) in &entries {
      let text = String::from_utf8_lossy(bytes);
      assert!(!text.contains(SECRET), "{name} still holds the secret: {text}");
      assert!(!text.contains(&encoded), "{name} still holds the encoded secret");
      assert!(
        !text.contains("hunter2-unknown"),
        "{name} still holds a password input's value"
      );
      assert!(!text.contains("session-cookie-value"), "{name} still holds a cookie");
      assert!(
        !bytes.windows(SECRET.len()).any(|window| window == SECRET.as_bytes()),
        "{name} still holds the secret's bytes"
      );
    }
    let trace = String::from_utf8_lossy(&entries[0].1);
    assert!(trace.contains(r#""value":"<secret>APP_PASSWORD</secret>""#));
  }

  #[test]
  fn form_bodies_lowercase_hex_and_json_escapes_are_found() {
    let r = Redactor::new([("APP_PASSWORD".to_owned(), "Sümmer \"2026\"!/x".to_owned())]);
    for spelling in [
      "S%C3%BCmmer+%222026%22%21%2Fx",
      "S%c3%bcmmer%20%222026%22!%2fx",
      r#"S\u00fcmmer \"2026\"!\/x"#,
      r#"Sümmer \"2026\"!/x"#,
    ] {
      assert_eq!(r.redact(spelling), "<secret>APP_PASSWORD</secret>", "{spelling}");
    }
  }

  #[test]
  fn a_har_loses_credentials_and_secrets_in_base64_bodies() {
    let body = base64::engine::general_purpose::STANDARD.encode(format!("token={SECRET}"));
    let mut har = serde_json::json!({ "log": { "entries": [{
      "request": { "headers": [{ "name": "Authorization", "value": "Basic dXNlcjpwYXNz" }], "cookies": [{ "name": "sid", "value": "abc" }] },
      "response": { "headers": [{ "name": "set-cookie", "value": "sid=abc" }, { "name": "x-note", "value": SECRET }],
        "content": { "encoding": "base64", "text": body } },
    }] } });
    redact_har(&mut har, &redactor());
    let entry = &har["log"]["entries"][0];
    assert_eq!(entry["request"]["headers"][0]["value"], BLANKED);
    assert_eq!(entry["request"]["cookies"][0]["value"], BLANKED);
    assert_eq!(entry["response"]["headers"][0]["value"], BLANKED);
    assert_eq!(
      entry["response"]["headers"][1]["value"],
      "<secret>APP_PASSWORD</secret>"
    );
    let decoded = base64::engine::general_purpose::STANDARD
      .decode(entry["response"]["content"]["text"].as_str().unwrap())
      .unwrap();
    assert_eq!(
      String::from_utf8(decoded).unwrap(),
      "token=<secret>APP_PASSWORD</secret>"
    );
  }
}
