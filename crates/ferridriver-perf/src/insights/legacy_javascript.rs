//! Polyfills and transforms shipped to browsers that don't need them.
//!
//! Mirrors devtools-frontend `insights/LegacyJavaScript.ts`, which wraps
//! `third_party/legacy-javascript`. The detection is the same: build one
//! regex per core-js polyfill from
//! [`super::polyfills::CORE_JS_POLYFILLS`], plus a handful of literal
//! strings that only appear in Babel's output, and match them against
//! the script source the trace carried.
//!
//! Upstream also uses the script's source map to attribute the exact
//! byte cost of each polyfill module. Source maps are not in the trace
//! and fetching them would make this crate do network I/O, so the byte
//! figure here is an estimate from how many patterns matched, and is
//! reported as such rather than as a measured saving.

use std::fmt::Write as _;
use std::sync::OnceLock;

use regex::RegexSet;
use rustc_hash::FxHashSet;

use crate::handlers::scripts::Script;
use crate::insights::polyfills::CORE_JS_POLYFILLS;
use crate::insights::{Check, Insight, Item, Severity};
use crate::units::{count_to_f64, len_to_f64};

/// Strings that appear only in the output of a specific Babel
/// transform. From `getTransformPatterns`.
const TRANSFORM_PATTERNS: [(&str, &str); 3] = [
  ("@babel/plugin-transform-classes", "Cannot call a class as a function"),
  (
    "@babel/plugin-transform-regenerator",
    "Generator is already running|regeneratorRuntime",
  ),
  (
    "@babel/plugin-transform-spread",
    "Invalid attempt to spread non-iterable instance",
  ),
];

/// Rough bytes a single polyfill module costs once shipped. Upstream
/// derives this per module from the source map; without one this is a
/// flat approximation, deliberately conservative.
const ESTIMATED_BYTES_PER_POLYFILL: i64 = 400;

/// The regex a polyfill is recognised by.
///
/// Replicates `buildPolyfillExpression`: an assignment to the property,
/// a bracketed assignment, a `defineProperty` call, a core-js `target`
/// descriptor, or an import of the core-js module itself.
#[must_use]
pub fn polyfill_expression(name: &str, core_js_module: &str) -> String {
  let (object, property) = match name.rsplit_once('.') {
    Some((object, property)) => (Some(object), property),
    None => (None, name),
  };
  let escaped_module = regex::escape(core_js_module);

  let mut expression = String::new();
  if let Some(object) = object {
    let object_re = regex::escape(object);
    let property_re = regex::escape(property);
    let bare_object = regex::escape(&object.replace(".prototype", ""));
    // `write!` into a String cannot fail; the result is discarded
    // rather than unwrapped so this stays free of panicking calls.
    let _ = write!(expression, r"{object_re}\.{property_re}\s?=[^=]");
    let _ = write!(expression, r#"|{object_re}\[['"]{property_re}['"]\]\s?=[^=]"#);
    let _ = write!(expression, r#"|defineProperty\({object_re},\s?['"]{property_re}['"]"#);
    let _ = write!(
      expression,
      r"|\({object_re},\s*\{{{property_re}:.*\}},\s*\{{{property_re}"
    );
    let _ = write!(
      expression,
      r#"|\{{target:['"]{bare_object}['"][^;]*\}},\{{{property_re}:"#
    );
  } else {
    let property_re = regex::escape(property);
    let _ = write!(expression, r"(?:window\.|[\s;]+){property_re}\s?=[^=]");
    let _ = write!(expression, r#"|defineProperty\(window,\s?['"]{property_re}['"]"#);
  }
  let _ = write!(expression, r#"|{escaped_module}(?:\.js)?""#);
  expression
}

/// One `RegexSet` over every pattern, compiled once. Matching 89
/// patterns separately against every script would be the slow way to
/// get the same answer.
fn matcher() -> &'static (RegexSet, Vec<String>) {
  static MATCHER: OnceLock<(RegexSet, Vec<String>)> = OnceLock::new();
  MATCHER.get_or_init(|| {
    let mut names = Vec::new();
    let mut patterns = Vec::new();
    for (name, module) in CORE_JS_POLYFILLS {
      names.push((*name).to_string());
      patterns.push(polyfill_expression(name, module));
    }
    for (name, pattern) in TRANSFORM_PATTERNS {
      names.push((*name).to_string());
      patterns.push((*pattern).to_string());
    }
    // A pattern that fails to compile is dropped rather than taking the
    // whole set down; the alternative is losing every other detection.
    let compiled: Vec<&str> = patterns.iter().map(String::as_str).collect();
    match RegexSet::new(&compiled) {
      Ok(set) => (set, names),
      Err(_) => (RegexSet::empty(), Vec::new()),
    }
  })
}

#[must_use]
pub fn run(scripts: &[Script]) -> Insight {
  let (set, names) = matcher();
  let mut items = Vec::new();
  let mut total_matches = 0usize;

  for script in scripts {
    let matched: FxHashSet<&str> = set
      .matches(&script.content)
      .into_iter()
      .filter_map(|i| names.get(i).map(String::as_str))
      .collect();
    if matched.is_empty() {
      continue;
    }
    total_matches += matched.len();
    let mut found: Vec<&str> = matched.into_iter().collect();
    found.sort_unstable();
    items.push(Item {
      label: format!(
        "{} ({})",
        if script.url.is_empty() { "(inline)" } else { &script.url },
        found.join(", ")
      ),
      value: len_to_f64(found.len()),
      unit: "patterns",
    });
  }

  items.sort_by(|a, b| b.value.total_cmp(&a.value));
  let passed = items.is_empty();
  let estimated_bytes = count_to_f64(
    i64::try_from(total_matches)
      .unwrap_or(i64::MAX)
      .saturating_mul(ESTIMATED_BYTES_PER_POLYFILL),
  );

  Insight {
    key: "LegacyJavaScript".into(),
    title: "Legacy JavaScript".into(),
    description: "Polyfills and transforms let older browsers use new JavaScript features, but many aren't \
                  necessary for modern browsers. Consider not transpiling Baseline features."
      .into(),
    severity: if passed { Severity::Pass } else { Severity::Fail },
    checks: vec![Check {
      name: "noLegacyJavaScript".into(),
      passed,
      detail: if passed {
        if scripts.is_empty() {
          "Not evaluated: the trace carries no script sources".into()
        } else {
          format!("No legacy polyfills or transforms in {} scripts", scripts.len())
        }
      } else {
        format!(
          "{} scripts ship {total_matches} legacy patterns (roughly {estimated_bytes:.0} bytes, estimated)",
          items.len()
        )
      },
    }],
    metrics: vec![("estimatedWastedBytes".into(), estimated_bytes)],
    items,
  }
}
