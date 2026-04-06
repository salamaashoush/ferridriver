//! Generate step definition skeletons for undefined Gherkin steps.
//!
//! When a scenario contains steps that have no matching step definition,
//! this module can produce a Rust function skeleton that the user can
//! copy-paste into their step definition file and fill in.

/// Generate a Rust step definition skeleton for an undefined Gherkin step.
///
/// # Arguments
///
/// * `keyword` - The Gherkin keyword (`Given`, `When`, `Then`, `And`, `But`, `*`).
/// * `text` - The step text (excluding the keyword), e.g. `I have 3 "apples" in my cart`.
/// * `has_table` - Whether the step has a DataTable argument.
/// * `has_docstring` - Whether the step has a DocString argument.
///
/// # Returns
///
/// A complete Rust function skeleton with `todo!()` body.
pub fn generate_snippet(keyword: &str, text: &str, has_table: bool, has_docstring: bool) -> String {
  // 1. Analyze the step text and build the expression pattern + parameter list.
  let (expression, params) = analyze_step_text(text);

  // 2. Generate a snake_case function name.
  let fn_name = to_snake_case_fn_name(&expression);

  // 3. Map keyword to attribute.
  let attr = keyword_to_attribute(keyword);

  // 4. Build parameter list.
  let mut all_params = vec!["world: &mut BrowserWorld".to_string()];
  for (i, param_type) in params.iter().enumerate() {
    all_params.push(format!("arg{i}: {param_type}"));
  }
  if has_table {
    all_params.push("table: Option<&DataTable>".to_string());
  }
  if has_docstring {
    all_params.push("docstring: Option<&str>".to_string());
  }

  let params_str = all_params.join(", ");

  format!(
    "#[{attr}(\"{expression}\")]\nasync fn {fn_name}({params_str}) {{\n  todo!(\"implement step\")\n}}"
  )
}

/// Analyze step text, replacing quoted strings, floats, and integers with
/// cucumber expression placeholders. Returns the expression pattern and a
/// list of Rust type strings for each placeholder.
fn analyze_step_text(text: &str) -> (String, Vec<&'static str>) {
  let mut params: Vec<&'static str> = Vec::new();
  let mut result = String::with_capacity(text.len());
  let chars: Vec<char> = text.chars().collect();
  let len = chars.len();
  let mut i = 0;

  while i < len {
    let ch = chars[i];

    // Detect quoted strings: "..." or '...'
    if ch == '"' || ch == '\'' {
      let quote = ch;
      // Find the closing quote.
      let mut j = i + 1;
      while j < len && chars[j] != quote {
        j += 1;
      }
      if j < len {
        // Found closing quote — replace entire quoted segment with {string}.
        result.push_str("{string}");
        params.push("String");
        i = j + 1;
        continue;
      }
      // No closing quote — treat as literal.
      result.push(ch);
      i += 1;
      continue;
    }

    // Detect numbers: integers and floats.
    // A number can start with an optional minus sign, but only if it's at a word boundary.
    if is_number_start(&chars, i, len) {
      let start = i;
      if chars[i] == '-' {
        i += 1;
      }
      // Consume digits.
      let digit_start = i;
      while i < len && chars[i].is_ascii_digit() {
        i += 1;
      }
      if i > digit_start {
        // Check for decimal point (float).
        if i < len && chars[i] == '.' && i + 1 < len && chars[i + 1].is_ascii_digit() {
          i += 1; // skip '.'
          while i < len && chars[i].is_ascii_digit() {
            i += 1;
          }
          // Make sure the number ends at a word boundary.
          if i >= len || !chars[i].is_alphanumeric() {
            result.push_str("{float}");
            params.push("f64");
            continue;
          }
        } else if i >= len || !chars[i].is_alphanumeric() {
          // Pure integer — ends at word boundary.
          result.push_str("{int}");
          params.push("i64");
          continue;
        }
      }
      // Not a valid standalone number — rewind and emit as literal.
      i = start;
      result.push(chars[i]);
      i += 1;
      continue;
    }

    result.push(ch);
    i += 1;
  }

  (result, params)
}

/// Check if position `i` in `chars` could be the start of a standalone number.
/// A number starts with a digit, or a `-` followed by a digit, and must be
/// preceded by a word boundary (start of string or non-alphanumeric char).
fn is_number_start(chars: &[char], i: usize, len: usize) -> bool {
  // Must be preceded by a word boundary.
  if i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_') {
    return false;
  }

  if chars[i].is_ascii_digit() {
    return true;
  }

  if chars[i] == '-' && i + 1 < len && chars[i + 1].is_ascii_digit() {
    return true;
  }

  false
}

/// Convert an expression pattern into a snake_case function name.
/// - Replace placeholder tokens like `{int}`, `{string}`, `{float}` with their names.
/// - Replace non-alphanumeric characters with `_`.
/// - Deduplicate underscores.
/// - Truncate to 60 characters.
/// - Trim leading/trailing underscores.
fn to_snake_case_fn_name(expression: &str) -> String {
  let mut result = String::with_capacity(expression.len());

  let chars: Vec<char> = expression.chars().collect();
  let len = chars.len();
  let mut i = 0;

  while i < len {
    if chars[i] == '{' {
      // Find the closing brace.
      if let Some(j) = chars[i..].iter().position(|&c| c == '}') {
        let placeholder: String = chars[i + 1..i + j].iter().collect();
        result.push_str(&placeholder);
        i += j + 1;
        continue;
      }
    }

    let ch = chars[i];
    if ch.is_alphanumeric() {
      result.push(ch.to_ascii_lowercase());
    } else {
      result.push('_');
    }
    i += 1;
  }

  // Deduplicate underscores.
  let mut deduped = String::with_capacity(result.len());
  let mut prev_underscore = false;
  for ch in result.chars() {
    if ch == '_' {
      if !prev_underscore {
        deduped.push('_');
      }
      prev_underscore = true;
    } else {
      deduped.push(ch);
      prev_underscore = false;
    }
  }

  // Trim leading/trailing underscores.
  let trimmed = deduped.trim_matches('_');

  // Truncate to 60 characters (at underscore boundary if possible).
  if trimmed.len() <= 60 {
    return trimmed.to_string();
  }

  let truncated = &trimmed[..60];
  // Try to cut at the last underscore to avoid splitting a word.
  if let Some(last_underscore) = truncated.rfind('_') {
    if last_underscore > 30 {
      return truncated[..last_underscore].to_string();
    }
  }
  truncated.to_string()
}

/// Map a Gherkin keyword to the appropriate proc-macro attribute name.
fn keyword_to_attribute(keyword: &str) -> &'static str {
  match keyword.trim() {
    "Given" => "given",
    "When" => "when",
    "Then" => "then",
    _ => "step",
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_basic_given() {
    let snippet = generate_snippet("Given", "I have 3 \"apples\" in my cart", false, false);
    assert!(snippet.contains("#[given(\"I have {int} {string} in my cart\")]"));
    assert!(snippet.contains("arg0: i64"));
    assert!(snippet.contains("arg1: String"));
    assert!(snippet.contains("async fn i_have_int_string_in_my_cart("));
    assert!(snippet.contains("todo!(\"implement step\")"));
  }

  #[test]
  fn test_when_keyword() {
    let snippet = generate_snippet("When", "I click the button", false, false);
    assert!(snippet.contains("#[when(\"I click the button\")]"));
  }

  #[test]
  fn test_then_keyword() {
    let snippet = generate_snippet("Then", "I should see the result", false, false);
    assert!(snippet.contains("#[then(\"I should see the result\")]"));
  }

  #[test]
  fn test_and_keyword_maps_to_step() {
    let snippet = generate_snippet("And", "something happens", false, false);
    assert!(snippet.contains("#[step(\"something happens\")]"));
  }

  #[test]
  fn test_but_keyword_maps_to_step() {
    let snippet = generate_snippet("But", "nothing breaks", false, false);
    assert!(snippet.contains("#[step(\"nothing breaks\")]"));
  }

  #[test]
  fn test_star_keyword_maps_to_step() {
    let snippet = generate_snippet("*", "do stuff", false, false);
    assert!(snippet.contains("#[step(\"do stuff\")]"));
  }

  #[test]
  fn test_float_detection() {
    let snippet = generate_snippet("Given", "the price is 3.14", false, false);
    assert!(snippet.contains("{float}"));
    assert!(snippet.contains("arg0: f64"));
  }

  #[test]
  fn test_negative_integer() {
    let snippet = generate_snippet("Given", "the temperature is -5 degrees", false, false);
    assert!(snippet.contains("{int}"));
    assert!(snippet.contains("arg0: i64"));
  }

  #[test]
  fn test_single_quoted_string() {
    let snippet = generate_snippet("When", "I type 'hello world'", false, false);
    assert!(snippet.contains("{string}"));
    assert!(snippet.contains("arg0: String"));
  }

  #[test]
  fn test_has_table() {
    let snippet = generate_snippet("Given", "a table of users", true, false);
    assert!(snippet.contains("table: Option<&DataTable>"));
  }

  #[test]
  fn test_has_docstring() {
    let snippet = generate_snippet("Given", "the following text", false, true);
    assert!(snippet.contains("docstring: Option<&str>"));
  }

  #[test]
  fn test_has_table_and_docstring() {
    let snippet = generate_snippet("Given", "data", true, true);
    assert!(snippet.contains("table: Option<&DataTable>"));
    assert!(snippet.contains("docstring: Option<&str>"));
  }

  #[test]
  fn test_fn_name_truncation() {
    let long_text = "a very long step definition text that exceeds the sixty character limit for function names in generated snippets";
    let snippet = generate_snippet("Given", long_text, false, false);
    // Extract the function name from `async fn <name>(`
    let fn_start = snippet.find("async fn ").unwrap() + "async fn ".len();
    let fn_end = snippet[fn_start..].find('(').unwrap() + fn_start;
    let fn_name = &snippet[fn_start..fn_end];
    assert!(fn_name.len() <= 60, "fn name too long: {fn_name} ({})", fn_name.len());
  }

  #[test]
  fn test_no_params_no_extras() {
    let snippet = generate_snippet("Given", "the app is running", false, false);
    assert!(snippet.contains("async fn the_app_is_running(world: &mut BrowserWorld)"));
  }

  #[test]
  fn test_multiple_params() {
    let snippet = generate_snippet("Given", "I have 5 \"items\" costing 9.99", false, false);
    assert!(snippet.contains("{int}"));
    assert!(snippet.contains("{string}"));
    assert!(snippet.contains("{float}"));
    assert!(snippet.contains("arg0: i64"));
    assert!(snippet.contains("arg1: String"));
    assert!(snippet.contains("arg2: f64"));
  }
}
