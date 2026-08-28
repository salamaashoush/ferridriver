//! Polyfills and transforms shipped to browsers that don't need them.
//!
//! Mirrors devtools-frontend `insights/LegacyJavaScript.ts`, which wraps
//! `third_party/legacy-javascript`. The detection is the same: build one
//! regex per core-js polyfill from
//! [`super::polyfills::CORE_JS_POLYFILLS`], plus a handful of literal
//! strings that only appear in Babel's output, and match them against
//! the script source the trace carried.
//!
//! The byte cost comes from upstream's own module-size graph: a
//! polyfill pulls in a set of core-js modules, and the cost is their
//! combined size counted once per script.
//!
//! One thing upstream does that this cannot: with a source map it also
//! finds polyfills by looking for core-js module paths among the
//! sources, catching ones whose emitted code no pattern matches. Maps
//! are not in the trace and fetching them would make this crate do
//! network I/O, so that second pass is absent and a heavily-minified
//! bundle may under-report.

use std::fmt::Write as _;
use std::sync::OnceLock;

use regex::RegexSet;
use rustc_hash::FxHashSet;

use crate::handlers::scripts::Script;
use crate::insights::polyfills::{CORE_JS_POLYFILLS, MAX_POLYFILL_SIZE, MODULE_SIZES, POLYFILL_DEPENDENCIES};
use crate::insights::{Check, Insight, Item, Severity};

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

/// Scripts smaller than this are not worth analysing, and neither are
/// savings smaller than this. Both gates are upstream's `BYTE_THRESHOLD`
/// and both matter: without them a few hundred bytes of hand-written
/// compatibility code is reported as a bundling problem.
const BYTE_THRESHOLD: usize = 5000;

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
  let mut total_bytes = 0u32;

  for script in scripts {
    // A script this small cannot be a transpiled bundle, and reporting
    // one is a false positive: a few hundred bytes of hand-written
    // compatibility code is not a bundling problem.
    if script.content.len() < BYTE_THRESHOLD {
      continue;
    }
    let matched: FxHashSet<&str> = set
      .matches(&script.content)
      .into_iter()
      .filter_map(|i| names.get(i).map(String::as_str))
      .collect();
    if matched.is_empty() {
      continue;
    }
    let wasted = wasted_bytes(&matched);
    if (wasted as usize) < BYTE_THRESHOLD {
      continue;
    }
    total_bytes = total_bytes.saturating_add(wasted);

    let mut found: Vec<&str> = matched.into_iter().collect();
    found.sort_unstable();
    items.push(Item {
      label: format!(
        "{} ({})",
        if script.url.is_empty() { "(inline)" } else { &script.url },
        found.join(", ")
      ),
      value: f64::from(wasted),
      unit: "bytes",
    });
  }

  items.sort_by(|a, b| b.value.total_cmp(&a.value));
  let passed = items.is_empty();
  let estimated_bytes = f64::from(total_bytes);

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
          "{} scripts ship legacy polyfills or transforms, about {:.0} KB of core-js",
          items.len(),
          estimated_bytes / 1024.0
        )
      },
    }],
    metrics: vec![("estimatedWastedBytes".into(), estimated_bytes)],
    items,
  }
}

/// Bytes the matched polyfills cost, from upstream's module-size graph.
///
/// Modules are counted ONCE across every polyfill in the script: two
/// polyfills that both pull in the same core-js internals do not pay for
/// it twice. Capped at the size of core-js itself.
fn wasted_bytes(matched: &FxHashSet<&str>) -> u32 {
  let mut modules: FxHashSet<usize> = FxHashSet::default();
  for name in matched {
    // Transform patterns are named `@babel/...` and have no core-js
    // modules behind them.
    if let Some((_, ids)) = POLYFILL_DEPENDENCIES.iter().find(|(key, _)| key == name) {
      modules.extend(ids.iter().copied());
    }
  }
  let total: u32 = modules.into_iter().filter_map(|id| MODULE_SIZES.get(id).copied()).sum();
  total.min(MAX_POLYFILL_SIZE)
}
