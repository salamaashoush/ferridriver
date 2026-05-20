//! Unified-diff rendering for assertion failures.
//!
//! Produces a plain-text diff (no ANSI) so it survives the
//! Rust → QuickJS → JS error round trip. Printers add color at output
//! time. The format matches the GNU `diff -u` shape — `-` for expected
//! lines (what we asked for), `+` for received (what the test got),
//! ` ` for context.

use similar::{ChangeTag, TextDiff};

use crate::asymmetric::Asymmetric;

/// Render a `serde_json::Value` as multi-line pretty JSON with
/// asymmetric matchers rendered as `<Description>` placeholders so the
/// diff highlights the matcher rather than its tagged wire shape.
pub fn pretty_json(v: &serde_json::Value) -> String {
  let humanized = humanize_asymmetric(v);
  serde_json::to_string_pretty(&humanized).unwrap_or_else(|_| humanized.to_string())
}

fn humanize_asymmetric(v: &serde_json::Value) -> serde_json::Value {
  if let Some(asym) = Asymmetric::from_value(v) {
    return serde_json::Value::String(asym.description());
  }
  match v {
    serde_json::Value::Array(arr) => serde_json::Value::Array(arr.iter().map(humanize_asymmetric).collect()),
    serde_json::Value::Object(map) => {
      let mut out = serde_json::Map::with_capacity(map.len());
      for (k, val) in map {
        out.insert(k.clone(), humanize_asymmetric(val));
      }
      serde_json::Value::Object(out)
    },
    other => other.clone(),
  }
}

/// Unified diff between two pretty-printed strings. Lines are prefixed
/// with `-` / `+` / ` `; empty when the two inputs are byte-identical.
pub fn unified_diff(expected: &str, received: &str) -> String {
  let diff = TextDiff::from_lines(expected, received);
  let mut out = String::new();
  for change in diff.iter_all_changes() {
    let sign = match change.tag() {
      ChangeTag::Delete => '-',
      ChangeTag::Insert => '+',
      ChangeTag::Equal => ' ',
    };
    out.push(sign);
    out.push_str(change.value().trim_end_matches('\n'));
    out.push('\n');
  }
  out.pop(); // drop final '\n' so multi-line messages don't double-blank.
  out
}

/// Render `expected` vs `received` as pretty JSON + a unified diff.
/// Suitable for `toEqual` / `toMatchObject` / `toContainEqual` where
/// the full structural shape matters.
pub fn json_diff(expected: &serde_json::Value, received: &serde_json::Value) -> String {
  let expected_str = pretty_json(expected);
  let received_str = pretty_json(received);
  unified_diff(&expected_str, &received_str)
}

#[cfg(test)]
mod tests {
  use super::*;
  use pretty_assertions::assert_eq;
  use serde_json::json;

  #[test]
  fn pretty_json_renders_multiline() {
    let v = json!({"a": 1, "b": [2, 3]});
    let s = pretty_json(&v);
    assert!(s.contains('\n'), "expected multi-line: {s}");
  }

  #[test]
  fn pretty_json_humanizes_asymmetric() {
    let v = json!({ "id": { crate::ASYM_TAG_KEY: "any", "name": "Number" } });
    let s = pretty_json(&v);
    assert!(s.contains("Any<Number>"), "humanized asym missing: {s}");
    assert!(!s.contains("@@asym"), "raw asym tag leaked: {s}");
  }

  #[test]
  fn unified_diff_marks_changed_lines() {
    let a = "line1\nline2\nline3";
    let b = "line1\nLINE2\nline3";
    let d = unified_diff(a, b);
    assert!(d.contains("-line2"), "missing '-' marker: {d}");
    assert!(d.contains("+LINE2"), "missing '+' marker: {d}");
    assert!(d.contains(" line1"), "missing context: {d}");
  }

  #[test]
  fn json_diff_for_object_mismatch() {
    let expected = json!({"a": 1, "b": 2});
    let actual = json!({"a": 1, "b": 3});
    let d = json_diff(&expected, &actual);
    assert!(d.contains('-'), "diff has no removals: {d}");
    assert!(d.contains('+'), "diff has no additions: {d}");
  }

  #[test]
  fn json_diff_empty_for_equal_values() {
    // Equal values still produce a fully-` `-prefixed diff (no -/+
    // lines) — useful so callers can assert "no real changes" by
    // checking whether any line starts with -/+.
    let v = json!({"a": 1});
    let d = json_diff(&v, &v);
    assert_eq!(
      d.lines().filter(|l| l.starts_with('-') || l.starts_with('+')).count(),
      0
    );
  }
}
