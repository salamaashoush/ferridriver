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

  /// Bytes that may or may not be text: redacted when they are UTF-8,
  /// returned untouched otherwise.
  #[must_use]
  pub fn redact_bytes<'a>(&self, bytes: &'a [u8]) -> Cow<'a, [u8]> {
    match std::str::from_utf8(bytes) {
      Ok(text) => match self.redact(text) {
        Cow::Borrowed(_) => Cow::Borrowed(bytes),
        Cow::Owned(text) => Cow::Owned(text.into_bytes()),
      },
      Err(_) => Cow::Borrowed(bytes),
    }
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

/// The spellings a value travels in: as is, percent-encoded the way a URL
/// or form body carries it, and base64 (basic auth, data URLs).
fn forms(value: &str) -> Vec<String> {
  let mut forms = vec![value.to_owned()];
  let encoded = percent_encode(value);
  if encoded != value {
    forms.push(encoded);
  }
  let plus = percent_encode(value).replace("%20", "+");
  if !forms.contains(&plus) {
    forms.push(plus);
  }
  forms.push(base64::engine::general_purpose::STANDARD.encode(value));
  forms.push(base64::engine::general_purpose::STANDARD_NO_PAD.encode(value));
  forms.dedup();
  forms
}

/// `encodeURIComponent`.
fn percent_encode(value: &str) -> String {
  let mut out = String::with_capacity(value.len());
  for byte in value.bytes() {
    if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
      out.push(char::from(byte));
    } else {
      use std::fmt::Write as _;
      let _ = write!(out, "%{byte:02X}");
    }
  }
  out
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

  let tmp = dst.with_extension("rewrite.tmp");
  let file = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
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
    } else if is_text(&name) {
      let mut bytes = Vec::new();
      entry.read_to_end(&mut bytes).map_err(|e| err(&e))?;
      writer.start_file(name.as_str(), options).map_err(|e| err(&e))?;
      writer.write_all(&redactor.redact_bytes(&bytes)).map_err(|e| err(&e))?;
    } else {
      drop(entry);
      let entry = archive.by_index_raw(index).map_err(|e| err(&e))?;
      writer.raw_copy_file(entry).map_err(|e| err(&e))?;
    }
    report.entries += 1;
  }
  writer.finish().map_err(|e| err(&e))?;
  std::fs::rename(&tmp, dst).map_err(|e| format!("{}: {e}", dst.display()))?;
  Ok(report)
}

/// Resources whose bytes are text a secret could appear in. Bodies are
/// named `<sha1>.<ext>` after their MIME type; sources `src@<sha1>.txt`.
fn is_text(name: &str) -> bool {
  let ext = Path::new(name)
    .extension()
    .and_then(|e| e.to_str())
    .map(str::to_ascii_lowercase);
  matches!(
    ext.as_deref(),
    Some("html" | "htm" | "js" | "mjs" | "css" | "json" | "txt" | "xml" | "svg" | "md" | "map" | "csv" | "dat")
  ) || !name.contains('.')
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
    let encoded = percent_encode(SECRET);
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
    let encoded = percent_encode(SECRET);
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
        "request": { "url": format!("https://a.test/?p={encoded}"), "headers": [{ "name": "x-token", "value": SECRET }],
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
      ],
    );
    let report = rewrite_zip(
      &path,
      &path,
      &redactor(),
      RewriteOptions {
        drop_media_bodies: true,
        drop_image_attachments: true,
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
        "resources/page@1-1.jpeg"
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
    }
    let trace = String::from_utf8_lossy(&entries[0].1);
    assert!(trace.contains(r#""value":"<secret>APP_PASSWORD</secret>""#));
  }
}
