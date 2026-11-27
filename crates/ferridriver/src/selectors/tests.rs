//! Unit tests for selector registry, parser, and query builders.

#[cfg(test)]
mod registry_tests {
  use super::super::{
    DEFAULT_TEST_ID_ATTRIBUTE, SelectorScript, default_test_id_attribute, evaluation_script,
    is_registered_selector_engine, register_selector_engine, registered_selector_engines, reset_selector_registry,
    set_default_test_id_attribute,
  };

  /// One test, not several: the registry is process-global, so separate
  /// `#[test]` fns would run concurrently and clobber each other.
  #[test]
  fn the_registry_is_playwrights_with_one_stated_difference() {
    reset_selector_registry();

    // Nothing registered: the hot path answers without touching the lock.
    assert!(!is_registered_selector_engine("tagname"));
    assert_eq!(default_test_id_attribute(), DEFAULT_TEST_ID_ATTRIBUTE);
    assert!(registered_selector_engines().is_empty());

    // Playwright's `evaluationScript`: a function is called with no
    // argument, a string is the expression itself.
    let engine =
      evaluation_script(&SelectorScript::Function("() => ({ queryAll: () => [] })".into())).expect("a function lowers");
    assert_eq!(engine, "(() => ({ queryAll: () => [] }))(undefined)");
    assert_eq!(
      evaluation_script(&SelectorScript::Source("({ queryAll: () => [] })".into())).expect("a string lowers"),
      "({ queryAll: () => [] })"
    );
    assert!(evaluation_script(&SelectorScript::Path("/definitely/not/here.js".into())).is_err());

    register_selector_engine("tagname", &engine, false).expect("first registration");
    assert!(is_registered_selector_engine("tagname"));
    // Playwright resolves `name:light` against the same engine.
    assert!(is_registered_selector_engine("tagname:light"));
    assert!(!is_registered_selector_engine("othername"));
    assert_eq!(registered_selector_engines().len(), 1);

    // The stated divergence: an IDENTICAL registration is a no-op,
    // because every worker in this process evaluates the same spec file.
    register_selector_engine("tagname", &engine, false).expect("an identical registration is a no-op");
    assert_eq!(registered_selector_engines().len(), 1);

    // A conflicting one still raises, with Playwright's message.
    let err = register_selector_engine("tagname", "({ queryAll: () => [1] })", false)
      .expect_err("a different script under the same name");
    assert!(
      err
        .to_string()
        .contains(r#"selectors.register: "tagname" selector engine has been already registered"#),
      "{err}"
    );

    set_default_test_id_attribute("data-pw");
    assert_eq!(default_test_id_attribute(), "data-pw");

    reset_selector_registry();
    assert_eq!(default_test_id_attribute(), DEFAULT_TEST_ID_ATTRIBUTE);
    assert!(!is_registered_selector_engine("tagname"));
  }
}

mod parser_tests {
  use super::super::*;

  fn single_engine(selector: &str) -> Engine {
    let parsed = parse(selector).unwrap();
    assert_eq!(parsed.parts.len(), 1, "expected one part for {selector}");
    parsed.parts[0].engine.clone()
  }

  #[test]
  fn bare_xpath_auto_detects() {
    assert_eq!(single_engine("//h1"), Engine::XPath);
    assert_eq!(single_engine("//*[@id='a' or @name='b']"), Engine::XPath);
    assert_eq!(single_engine("(//h1)[1]"), Engine::XPath);
    assert_eq!(single_engine("((//a)[2])"), Engine::XPath);
    assert_eq!(single_engine("../div"), Engine::XPath);
  }

  #[test]
  fn quoted_text_auto_detects_and_keeps_quotes() {
    let parsed = parse("\"Sign In\"").unwrap();
    assert_eq!(parsed.parts[0].engine, Engine::Text);
    assert_eq!(parsed.parts[0].body, "\"Sign In\"");
    assert_eq!(single_engine("'Sign In'"), Engine::Text);
  }

  #[test]
  fn css_still_default() {
    assert_eq!(single_engine("h1"), Engine::Css);
    assert_eq!(single_engine("div.cls > a"), Engine::Css);
    assert_eq!(single_engine("[data-x='//not-xpath']"), Engine::Css);
  }

  #[test]
  fn explicit_prefixes_untouched() {
    assert_eq!(single_engine("xpath=//h1"), Engine::XPath);
    assert_eq!(single_engine("css=//weird"), Engine::Css);
    assert_eq!(single_engine("text=//literal"), Engine::Text);
  }

  #[test]
  fn chained_parts_detect_each() {
    let parsed = parse("div.row >> //button").unwrap();
    assert_eq!(parsed.parts[0].engine, Engine::Css);
    assert_eq!(parsed.parts[1].engine, Engine::XPath);
  }

  #[test]
  fn rich_selector_covers_auto_detected() {
    assert!(is_rich_selector("//h1"));
    assert!(is_rich_selector("(//h1)[1]"));
    assert!(is_rich_selector("../div"));
    assert!(is_rich_selector("\"Sign In\""));
    assert!(is_rich_selector("data-testid=submit"));
    assert!(is_rich_selector("*css=div >> text=x"));
    assert!(!is_rich_selector("h1.cls"));
    assert!(!is_rich_selector("input[name=login]"));
  }

  #[test]
  fn engine_name_with_surrounding_whitespace() {
    // Playwright trims the name (`part.substring(0, eqIndex).trim()`)
    // and keeps the body untrimmed.
    let parsed = parse("div >> text =  hi").unwrap();
    assert_eq!(parsed.parts[1].engine, Engine::Text);
    assert_eq!(parsed.parts[1].body, "  hi");
  }

  #[test]
  fn unknown_engine_errors_like_playwright() {
    let err = parse("bogus=stuff").unwrap_err();
    assert!(err.to_string().contains("unknown selector engine"), "{err}");
  }

  #[test]
  fn capture_modifier_parses_once() {
    let parsed = parse("*css=div >> text=x").unwrap();
    assert!(parsed.parts[0].capture);
    assert!(!parsed.parts[1].capture);
    let err = parse("*css=div >> *text=x").unwrap_err();
    assert!(err.to_string().contains("capture"), "{err}");
  }

  #[test]
  fn playwright_native_engines_pass_through() {
    assert_eq!(single_engine("data-testid=submit"), Engine::Other("data-testid".into()));
    assert_eq!(single_engine("aria-ref=e12"), Engine::Other("aria-ref".into()));
    assert_eq!(single_engine("css:light=div"), Engine::Other("css:light".into()));
  }

  #[test]
  fn text_body_quotes_do_not_swallow_chain() {
    // Inside a non-empty text= body, quotes are literal (Playwright's
    // shouldIgnoreTextSelectorQuote) — the chain after must still split.
    let parsed = parse("text=Don't stop >> css=div").unwrap();
    assert_eq!(parsed.parts.len(), 2);
    assert_eq!(parsed.parts[0].engine, Engine::Text);
    assert_eq!(parsed.parts[0].body, "Don't stop");
    assert_eq!(parsed.parts[1].engine, Engine::Css);
  }

  #[test]
  fn backtick_quotes_protect_chain() {
    let parsed = parse("css=[data-x=`a >> b`] >> text=x").unwrap();
    assert_eq!(parsed.parts.len(), 2);
    assert_eq!(parsed.parts[0].body, "[data-x=`a >> b`]");
  }

  #[test]
  fn capture_survives_parts_json() {
    let parsed = parse("*css=div >> text=x").unwrap();
    let json = build_parts_json(&parsed);
    assert!(json.contains(r#""capture":true"#), "{json}");
  }

  #[test]
  fn a_read_tells_no_match_from_a_null_value_and_a_thrown_read() {
    let decode = |value: serde_json::Value| decode_read_result(Some(value), "#x");
    assert_eq!(decode(serde_json::json!({ MISSING_KEY: 1 })).unwrap(), None);
    assert_eq!(
      decode(serde_json::json!({ VALUE_KEY: null })).unwrap(),
      Some(serde_json::Value::Null)
    );
    assert_eq!(
      decode(serde_json::json!("text")).unwrap(),
      Some(serde_json::json!("text"))
    );
    assert!(
      decode(serde_json::json!({ STRICT_KEY: 2 }))
        .unwrap_err()
        .is_strict_mode_violation()
    );
    assert!(matches!(
      decode(serde_json::json!({ ERROR_KEY: "Not a checkbox or radio button" })).unwrap_err(),
      FerriError::Evaluation(message) if message.contains("Not a checkbox")
    ));
  }
}
