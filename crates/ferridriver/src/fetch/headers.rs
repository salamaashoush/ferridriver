//! WHATWG header list.
//!
//! An ordered `(name, value)` list with case-insensitive lookup. `get`
//! combines same-name values with `, ` (per WHATWG), except `set-cookie`
//! which combines with `\n` and is also readable split via
//! [`Headers::get_set_cookie`] — matching Playwright's `RawHeaders`
//! (`client/network.ts:931`). Insertion order and the original name
//! casing are preserved for `iter`, so the engine ships headers to
//! reqwest exactly as assembled.

/// An ordered, case-insensitive header list.
#[derive(Debug, Clone, Default)]
pub struct Headers {
  entries: Vec<(String, String)>,
}

impl Headers {
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  /// Build from an ordered `(name, value)` list verbatim (duplicates and
  /// casing preserved) — the shape reqwest response headers arrive in.
  #[must_use]
  pub fn from_pairs(entries: Vec<(String, String)>) -> Self {
    Self { entries }
  }

  fn position(&self, name: &str) -> Option<usize> {
    self.entries.iter().position(|(k, _)| k.eq_ignore_ascii_case(name))
  }

  /// WHATWG combined value: all same-name values joined with `, `
  /// (`\n` for `set-cookie`). `None` when the header is absent.
  #[must_use]
  pub fn get(&self, name: &str) -> Option<String> {
    let values = self.get_all(name);
    if values.is_empty() {
      return None;
    }
    let sep = if name.eq_ignore_ascii_case("set-cookie") {
      "\n"
    } else {
      ", "
    };
    Some(values.join(sep))
  }

  /// The first value for `name`, uncombined (case-insensitive).
  #[must_use]
  pub fn get_first(&self, name: &str) -> Option<&str> {
    self.position(name).map(|i| self.entries[i].1.as_str())
  }

  /// Every value stored under `name`, in insertion order.
  #[must_use]
  pub fn get_all(&self, name: &str) -> Vec<&str> {
    self
      .entries
      .iter()
      .filter(|(k, _)| k.eq_ignore_ascii_case(name))
      .map(|(_, v)| v.as_str())
      .collect()
  }

  /// The `Set-Cookie` values, kept individually (WHATWG `getSetCookie`).
  #[must_use]
  pub fn get_set_cookie(&self) -> Vec<&str> {
    self.get_all("set-cookie")
  }

  #[must_use]
  pub fn contains(&self, name: &str) -> bool {
    self.position(name).is_some()
  }

  /// Replace every value for `name` with a single `value` (WHATWG
  /// `set`). The first slot keeps its position; later duplicates drop.
  pub fn set(&mut self, name: &str, value: impl Into<String>) {
    let value = value.into();
    match self.position(name) {
      Some(i) => {
        self.entries[i].1 = value;
        // Drop any later duplicates so `set` truly replaces all.
        let lower = name.to_ascii_lowercase();
        let mut seen = false;
        self.entries.retain(|(k, _)| {
          if k.eq_ignore_ascii_case(&lower) {
            let keep = !seen;
            seen = true;
            keep
          } else {
            true
          }
        });
      },
      None => self.entries.push((name.to_string(), value)),
    }
  }

  /// Set `name` to `value` only if it is not already present.
  pub fn set_if_absent(&mut self, name: &str, value: impl Into<String>) {
    if !self.contains(name) {
      self.entries.push((name.to_string(), value.into()));
    }
  }

  /// Append a value under `name` without removing existing ones (WHATWG
  /// `append`).
  pub fn append(&mut self, name: impl Into<String>, value: impl Into<String>) {
    self.entries.push((name.into(), value.into()));
  }

  /// Remove every value stored under `name`.
  pub fn remove(&mut self, name: &str) {
    self.entries.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
  }

  /// Iterate the `(name, value)` entries in insertion order.
  pub fn iter(&self) -> impl Iterator<Item = &(String, String)> {
    self.entries.iter()
  }

  #[must_use]
  pub fn entries(&self) -> &[(String, String)] {
    &self.entries
  }

  #[must_use]
  pub fn into_pairs(self) -> Vec<(String, String)> {
    self.entries
  }

  /// Flattened header object: lowercased names, each mapped to its
  /// combined value, ordered by first appearance. Playwright's
  /// `RawHeaders.headers()` (`client/network.ts:959`), which backs
  /// `apiResponse.headers()`.
  #[must_use]
  pub fn to_object(&self) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (name, _) in &self.entries {
      let lower = name.to_ascii_lowercase();
      if out.iter().any(|(k, _)| *k == lower) {
        continue;
      }
      let combined = self.get(&lower).unwrap_or_default();
      out.push((lower, combined));
    }
    out
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.entries.is_empty()
  }
}

impl From<Vec<(String, String)>> for Headers {
  fn from(entries: Vec<(String, String)>) -> Self {
    Self::from_pairs(entries)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn combined_get_and_set_cookie_split() {
    let mut h = Headers::new();
    h.append("Accept", "a");
    h.append("accept", "b");
    assert_eq!(h.get("accept").as_deref(), Some("a, b"));
    h.append("Set-Cookie", "x=1");
    h.append("set-cookie", "y=2");
    assert_eq!(h.get("set-cookie").as_deref(), Some("x=1\ny=2"));
    assert_eq!(h.get_set_cookie(), vec!["x=1", "y=2"]);
  }

  #[test]
  fn to_object_lowercases_combines_and_keeps_first_appearance_order() {
    let h = Headers::from_pairs(vec![
      ("Content-Type".into(), "text/plain".into()),
      ("Set-Cookie".into(), "a=1".into()),
      ("X-Dup".into(), "one".into()),
      ("set-cookie".into(), "b=2".into()),
      ("x-dup".into(), "two".into()),
    ]);
    assert_eq!(
      h.to_object(),
      vec![
        ("content-type".to_string(), "text/plain".to_string()),
        ("set-cookie".to_string(), "a=1\nb=2".to_string()),
        ("x-dup".to_string(), "one, two".to_string()),
      ]
    );
  }

  #[test]
  fn set_replaces_all_same_name() {
    let mut h = Headers::from_pairs(vec![
      ("x".into(), "1".into()),
      ("y".into(), "2".into()),
      ("X".into(), "3".into()),
    ]);
    h.set("x", "9");
    assert_eq!(h.get_all("x"), vec!["9"]);
    // Order of the first slot is preserved, other headers untouched.
    assert_eq!(h.entries()[0], ("x".into(), "9".into()));
    assert_eq!(h.get_first("y"), Some("2"));
  }

  #[test]
  fn set_if_absent_and_remove() {
    let mut h = Headers::new();
    h.set_if_absent("content-type", "application/json");
    h.set_if_absent("content-type", "text/plain");
    assert_eq!(h.get_first("content-type"), Some("application/json"));
    h.remove("content-type");
    assert!(!h.contains("content-type"));
  }
}
