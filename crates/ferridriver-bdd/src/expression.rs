//! Cucumber expression compiler: converts cucumber expressions to regex with typed parameters.
//!
//! Uses the `cucumber-expressions` crate's built-in `Expression::regex()` method
//! to compile patterns like `"I have {int} item(s) in my cart"` into regex.

use cucumber_expressions::Expression;
use regex::Regex;

use crate::param_type::ParameterTypeRegistry;
use crate::step::StepParam;

/// Parameter type expected from a cucumber expression capture group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamType {
  String,
  Int,
  Float,
  Word,
  /// Anonymous capture group.
  Anonymous,
  /// Custom parameter type registered via `ParameterTypeRegistry`.
  Custom(std::string::String),
}

/// A parameter with its type and the unique ID assigned by the parser.
///
/// The ID is needed because `{string}` parameters use named capture groups
/// `__N_0` and `__N_1` (double-quoted and single-quoted variants) where N is the
/// parameter's `.id` field from the AST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamInfo {
  pub ty: ParamType,
  pub id: usize,
}

/// A compiled cucumber expression ready for matching.
pub struct CompiledExpression {
  /// The compiled regex.
  pub regex: Regex,
  /// Expected parameter types in capture group order.
  pub param_types: Vec<ParamType>,
  /// Full parameter info (type + id) in capture group order.
  pub param_infos: Vec<ParamInfo>,
}

/// Compile a cucumber expression into a regex with typed parameters.
///
/// When `custom_types` is provided, unknown parameter names are looked up in the
/// registry and their regex patterns are substituted before compilation.
pub fn compile(expression: &str) -> Result<CompiledExpression, String> {
  static EMPTY_REGISTRY: std::sync::LazyLock<ParameterTypeRegistry> =
    std::sync::LazyLock::new(ParameterTypeRegistry::new);
  compile_with_custom(expression, &EMPTY_REGISTRY)
}

/// Compile a cucumber expression with a custom parameter type registry.
pub fn compile_with_custom(
  expression: &str,
  custom_types: &ParameterTypeRegistry,
) -> Result<CompiledExpression, String> {
  // Parse the expression AST to extract parameter types and IDs.
  let parsed = Expression::parse(expression)
    .map_err(|e| format!("invalid cucumber expression \"{expression}\": {e}"))?;

  let mut param_infos = Vec::new();
  extract_param_types(&parsed, custom_types, &mut param_infos);

  let param_types: Vec<ParamType> = param_infos.iter().map(|p| p.ty.clone()).collect();

  // For custom parameter types, pre-process the expression string by replacing
  // `{custom_name}` with the registered regex pattern wrapped in a capture group,
  // because the cucumber-expressions crate doesn't know about custom types.
  let has_custom = param_types.iter().any(|t| matches!(t, ParamType::Custom(_)));
  let regex = if has_custom {
    let mut processed = expression.to_string();
    for info in &param_infos {
      if let ParamType::Custom(ref name) = info.ty {
        if let Some(custom) = custom_types.find(name) {
          let placeholder = format!("{{{name}}}");
          let replacement = format!("({})", custom.regex);
          processed = processed.replacen(&placeholder, &replacement, 1);
        }
      }
    }
    // Now compile the processed expression (which has custom params replaced with
    // raw regex groups) through the cucumber-expressions crate. But since the
    // custom params are now raw regex, we need to handle the remaining standard
    // cucumber parts. Re-parse the modified expression.
    Expression::regex(&processed)
      .map_err(|e| {
        // If cucumber-expressions can't handle the modified expression, fall back
        // to building a regex directly from the processed string.
        tracing::debug!("cucumber-expressions failed on processed expression, building regex directly: {e}");
        e
      })
      .or_else(|_| {
        // Build the regex by hand: anchor the processed expression.
        Regex::new(&format!("^{processed}$"))
          .map_err(|e| format!("failed to compile processed expression \"{processed}\": {e}"))
      })?
  } else {
    Expression::regex(expression)
      .map_err(|e| format!("failed to compile expression \"{expression}\": {e}"))?
  };

  Ok(CompiledExpression { regex, param_types, param_infos })
}

fn extract_param_types(
  expr: &Expression<cucumber_expressions::Spanned<'_>>,
  custom_types: &ParameterTypeRegistry,
  params: &mut Vec<ParamInfo>,
) {
  for single in expr.iter() {
    if let cucumber_expressions::SingleExpression::Parameter(p) = single {
      let name: &str = *p.input;
      let ty = match name {
        "string" => ParamType::String,
        "int" => ParamType::Int,
        "float" => ParamType::Float,
        "word" => ParamType::Word,
        "" => ParamType::Anonymous,
        _ => {
          if custom_types.find(name).is_some() {
            ParamType::Custom(name.to_string())
          } else {
            ParamType::Anonymous
          }
        }
      };
      params.push(ParamInfo { ty, id: p.id });
    }
    // Alternations and optionals don't produce capture groups.
  }
}

/// Extract typed parameters from regex captures using the expected param types.
///
/// For `{string}` parameters, the cucumber-expressions crate generates named
/// capture groups `__N_0` (double-quoted) and `__N_1` (single-quoted) instead of
/// simple positional groups. We check both named groups and use whichever matched.
///
/// For `{int}`, `{float}`, `{word}`, and `{}`, simple positional capture groups
/// are used, so we fall back to `captures.get(group_index)`.
pub fn extract_params(
  captures: &regex::Captures<'_>,
  types: &[ParamType],
  infos: &[ParamInfo],
) -> Result<Vec<StepParam>, String> {
  extract_params_with_custom(captures, types, infos, None)
}

/// Extract typed parameters from regex captures, with optional custom type registry
/// for applying transformers.
pub fn extract_params_with_custom(
  captures: &regex::Captures<'_>,
  types: &[ParamType],
  infos: &[ParamInfo],
  custom_types: Option<&ParameterTypeRegistry>,
) -> Result<Vec<StepParam>, String> {
  let mut params = Vec::with_capacity(types.len());

  // Track positional group index. For non-string params each param uses one
  // positional group. For string params, two named groups are emitted (no
  // positional group). We need to figure out the right positional index.
  //
  // Actually, looking at the regex output: `{int}` emits `((?:-?\d+)|(?:\d+))`
  // which is a single outer capturing group. `{string}` emits a non-capturing
  // `(?:...)` wrapper with two named groups inside. So string params don't
  // consume a positional group index, but non-string params do.
  let mut positional_index = 1_usize; // skip full match at index 0

  for info in infos {
    let param = match &info.ty {
      ParamType::String => {
        // Look for named groups __N_0 (double-quoted) or __N_1 (single-quoted).
        // These named groups are still capturing groups that consume 2 positional
        // indices (one per quote variant), so advance the positional counter.
        let name0 = format!("__{}_0", info.id);
        let name1 = format!("__{}_1", info.id);
        let cap = captures
          .name(&name0)
          .or_else(|| captures.name(&name1))
          .map(|m| m.as_str())
          .unwrap_or("");
        positional_index += 2;
        StepParam::String(cap.to_string())
      }
      ParamType::Int => {
        let cap = captures
          .get(positional_index)
          .map(|m| m.as_str())
          .unwrap_or("");
        positional_index += 1;
        let val = cap
          .parse::<i64>()
          .map_err(|e| format!("failed to parse int param \"{cap}\": {e}"))?;
        StepParam::Int(val)
      }
      ParamType::Float => {
        let cap = captures
          .get(positional_index)
          .map(|m| m.as_str())
          .unwrap_or("");
        positional_index += 1;
        let val = cap
          .parse::<f64>()
          .map_err(|e| format!("failed to parse float param \"{cap}\": {e}"))?;
        StepParam::Float(val)
      }
      ParamType::Word | ParamType::Anonymous => {
        let cap = captures
          .get(positional_index)
          .map(|m| m.as_str())
          .unwrap_or("");
        positional_index += 1;
        StepParam::Word(cap.to_string())
      }
      ParamType::Custom(name) => {
        let cap = captures
          .get(positional_index)
          .map(|m| m.as_str())
          .unwrap_or("");
        positional_index += 1;
        // If a transformer is registered, use it; otherwise return Custom variant.
        if let Some(registry) = custom_types {
          if let Some(custom) = registry.find(name) {
            if let Some(ref transformer) = custom.transformer {
              transformer(cap)
            } else {
              StepParam::Custom { type_name: name.clone(), value: cap.to_string() }
            }
          } else {
            StepParam::Custom { type_name: name.clone(), value: cap.to_string() }
          }
        } else {
          StepParam::Custom { type_name: name.clone(), value: cap.to_string() }
        }
      }
    };

    params.push(param);
  }

  Ok(params)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn compile_simple_string() {
    let expr = compile("I navigate to {string}").unwrap();
    assert!(expr.regex.is_match("I navigate to \"https://example.com\""));
    assert_eq!(expr.param_types, vec![ParamType::String]);
  }

  #[test]
  fn compile_int() {
    let expr = compile("I wait {int} seconds").unwrap();
    assert!(expr.regex.is_match("I wait 5 seconds"));
    assert_eq!(expr.param_types, vec![ParamType::Int]);
  }

  #[test]
  fn compile_optional() {
    let expr = compile("I have {int} item(s)").unwrap();
    assert!(expr.regex.is_match("I have 1 item"));
    assert!(expr.regex.is_match("I have 5 items"));
    assert_eq!(expr.param_types, vec![ParamType::Int]);
  }

  #[test]
  fn compile_multiple_params() {
    let expr = compile("I fill {string} with {string}").unwrap();
    assert!(expr.regex.is_match("I fill \"#input\" with \"hello\""));
    assert_eq!(expr.param_types, vec![ParamType::String, ParamType::String]);
  }

  #[test]
  fn extract_string_param() {
    let expr = compile("I navigate to {string}").unwrap();
    let caps = expr.regex.captures("I navigate to \"https://example.com\"").unwrap();
    let params = extract_params(&caps, &expr.param_types, &expr.param_infos).unwrap();
    assert_eq!(params.len(), 1);
    assert_eq!(params[0], StepParam::String("https://example.com".to_string()));
  }

  #[test]
  fn extract_single_quoted_string_param() {
    let expr = compile("I navigate to {string}").unwrap();
    let caps = expr.regex.captures("I navigate to 'https://example.com'").unwrap();
    let params = extract_params(&caps, &expr.param_types, &expr.param_infos).unwrap();
    assert_eq!(params.len(), 1);
    assert_eq!(params[0], StepParam::String("https://example.com".to_string()));
  }

  #[test]
  fn extract_multiple_string_params() {
    let expr = compile("I fill {string} with {string}").unwrap();
    let caps = expr.regex.captures("I fill \"#input\" with \"hello\"").unwrap();
    let params = extract_params(&caps, &expr.param_types, &expr.param_infos).unwrap();
    assert_eq!(params.len(), 2);
    assert_eq!(params[0], StepParam::String("#input".to_string()));
    assert_eq!(params[1], StepParam::String("hello".to_string()));
  }

  #[test]
  fn extract_int_param() {
    let expr = compile("I wait {int} seconds").unwrap();
    let caps = expr.regex.captures("I wait 5 seconds").unwrap();
    let params = extract_params(&caps, &expr.param_types, &expr.param_infos).unwrap();
    assert_eq!(params.len(), 1);
    assert_eq!(params[0], StepParam::Int(5));
  }

  #[test]
  fn extract_mixed_params() {
    let expr = compile("I fill {string} with {int} items").unwrap();
    let caps = expr.regex.captures("I fill \"cart\" with 3 items").unwrap();
    let params = extract_params(&caps, &expr.param_types, &expr.param_infos).unwrap();
    assert_eq!(params.len(), 2);
    assert_eq!(params[0], StepParam::String("cart".to_string()));
    assert_eq!(params[1], StepParam::Int(3));
  }
}
