//! Cucumber expression compiler: converts cucumber expressions to regex with typed parameters.
//!
//! Patterns like `"I have {int} item(s) in my cart"` are parsed into the
//! cucumber-expressions AST and expanded into a regex from that AST. Custom
//! parameter types are substituted during expansion (through the crate's
//! `ParametersProvider`), never by splicing their regex into the expression
//! source — a spliced `red|green|blue` re-parses as an optional group and
//! silently compiles into something that matches nothing.

use cucumber_expressions::expand::{IntoRegexCharIter, ParameterError, ParametersProvider};
use cucumber_expressions::{Expression, Spanned};
use ferridriver::FerriError;
use ferridriver::error::Result;
use regex::Regex;

use crate::param_type::ParameterTypeRegistry;
use crate::step::StepParam;

fn invalid_expr(reason: impl Into<String>) -> FerriError {
  FerriError::invalid_argument("cucumber-expression", reason)
}

/// Parameter names the cucumber-expressions expander handles itself.
pub const BUILTIN_PARAM_TYPES: [&str; 5] = ["", "int", "float", "word", "string"];

/// Custom parameter types, resolved during AST expansion.
///
/// Built-in names resolve to `None` so the expander keeps its own definitions,
/// which is the same precedence [`param_type_of`] applies.
#[derive(Clone, Copy)]
struct RegistryParameters<'r>(&'r ParameterTypeRegistry);

impl<'r, 's> ParametersProvider<Spanned<'s>> for RegistryParameters<'r> {
  type Item = char;
  type Value = &'r str;

  fn get(&self, input: &Spanned<'s>) -> Option<Self::Value> {
    let name: &str = input.fragment();
    if BUILTIN_PARAM_TYPES.contains(&name) {
      return None;
    }
    self.0.find(name).map(|custom| custom.regex.as_str())
  }
}

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

/// Where a parameter's value lives in the compiled regex.
///
/// A parameter expands either to one positional group, or — when its matcher
/// carries capture groups of its own — to a run of named groups
/// `__{id}_{n}`, of which the one that participated in the match holds the
/// value. `{string}` is the built-in case of the latter (`__{id}_0` for the
/// double-quoted variant, `__{id}_1` for the single-quoted one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamSlot {
  Positional(usize),
  Named(Vec<String>),
}

/// A parameter with its type, the unique ID assigned by the parser, and the
/// capture groups it owns in the compiled regex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamInfo {
  pub ty: ParamType,
  pub id: usize,
  pub slot: ParamSlot,
}

impl ParamInfo {
  /// A parameter backed by a single positional capture group.
  pub fn positional(ty: ParamType, id: usize, group: usize) -> Self {
    Self {
      ty,
      id,
      slot: ParamSlot::Positional(group),
    }
  }
}

/// A compiled cucumber expression ready for matching.
#[derive(Debug)]
pub struct CompiledExpression {
  /// The compiled regex.
  pub regex: Regex,
  /// Expected parameter types in capture group order.
  pub param_types: Vec<ParamType>,
  /// Full parameter info (type + id) in capture group order.
  pub param_infos: Vec<ParamInfo>,
}

/// Compile a cucumber expression into a regex with typed parameters,
/// recognising only the built-in parameter types.
pub fn compile(expression: &str) -> Result<CompiledExpression> {
  static EMPTY_REGISTRY: std::sync::LazyLock<ParameterTypeRegistry> =
    std::sync::LazyLock::new(ParameterTypeRegistry::new);
  compile_with_custom(expression, &EMPTY_REGISTRY)
}

/// Compile a cucumber expression with a custom parameter type registry.
pub fn compile_with_custom(expression: &str, custom_types: &ParameterTypeRegistry) -> Result<CompiledExpression> {
  let parsed = Expression::parse(expression)
    .map_err(|e| invalid_expr(format!("invalid cucumber expression \"{expression}\": {e}")))?;

  let declared: Vec<(ParamType, usize)> = parsed
    .iter()
    .filter_map(|single| match single {
      cucumber_expressions::SingleExpression::Parameter(p) => Some((param_type_of(p.input.fragment()), p.id)),
      // Alternations, optionals and text don't produce capture groups.
      _ => None,
    })
    .collect();

  let pattern = parsed
    .with_parameters(RegistryParameters(custom_types))
    .into_regex_char_iter()
    .collect::<std::result::Result<String, ParameterError<Spanned<'_>>>>()
    .map_err(|e| match e {
      ParameterError::NotFound(name) => invalid_expr(format!(
        "undefined parameter type {{{name}}} in cucumber expression \"{expression}\"; \
         register it with defineParameterType before the step that uses it"
      )),
      ParameterError::RenameRegexGroup { parameter, re, err } => invalid_expr(format!(
        "parameter type {{{parameter}}} in cucumber expression \"{expression}\" has an invalid regex \"{re}\": {err}"
      )),
    })?;

  let regex = Regex::new(&pattern).map_err(|e| {
    invalid_expr(format!(
      "cucumber expression \"{expression}\" compiled to an \
       invalid regex \"{pattern}\": {e}"
    ))
  })?;

  let param_infos = assign_slots(expression, &regex, declared)?;
  let param_types: Vec<ParamType> = param_infos.iter().map(|p| p.ty.clone()).collect();

  Ok(CompiledExpression {
    regex,
    param_types,
    param_infos,
  })
}

fn param_type_of(name: &str) -> ParamType {
  match name {
    "string" => ParamType::String,
    "int" => ParamType::Int,
    "float" => ParamType::Float,
    "word" => ParamType::Word,
    "" => ParamType::Anonymous,
    other => ParamType::Custom(other.to_string()),
  }
}

/// Map every declared parameter onto the capture groups the expander emitted
/// for it, in order. A parameter owns either a run of `__{id}_{n}` named
/// groups or exactly one unnamed positional group; anything else means the
/// expansion and the AST walk disagree, which is a hard error rather than a
/// silently shifted parameter list.
fn assign_slots(expression: &str, regex: &Regex, declared: Vec<(ParamType, usize)>) -> Result<Vec<ParamInfo>> {
  let names: Vec<Option<String>> = regex.capture_names().map(|n| n.map(str::to_string)).collect();
  let mut infos = Vec::with_capacity(declared.len());
  let mut group = 1_usize;

  for (ty, id) in declared {
    let prefix = format!("__{id}_");
    let mut named = Vec::new();
    while let Some(Some(name)) = names.get(group) {
      if !name.starts_with(&prefix) {
        break;
      }
      named.push(name.clone());
      group += 1;
    }

    let slot = if named.is_empty() {
      match names.get(group) {
        Some(None) => {
          let at = group;
          group += 1;
          ParamSlot::Positional(at)
        },
        Some(Some(other)) => {
          return Err(invalid_expr(format!(
            "cucumber expression \"{expression}\" expanded parameter #{id} onto capture group \"{other}\", \
             which belongs to another parameter"
          )));
        },
        None => {
          return Err(invalid_expr(format!(
            "cucumber expression \"{expression}\" expanded parameter #{id} onto no capture group"
          )));
        },
      }
    } else {
      ParamSlot::Named(named)
    };

    infos.push(ParamInfo { ty, id, slot });
  }

  Ok(infos)
}

/// Extract typed parameters from regex captures using the expected param types.
///
/// Each parameter reads the capture groups [`assign_slots`] mapped onto it at
/// compile time.
pub fn extract_params(
  captures: &regex::Captures<'_>,
  types: &[ParamType],
  infos: &[ParamInfo],
) -> Result<Vec<StepParam>> {
  extract_params_with_custom(captures, types, infos, None)
}

/// Extract typed parameters from regex captures, with optional custom type registry
/// for applying transformers.
pub fn extract_params_with_custom(
  captures: &regex::Captures<'_>,
  types: &[ParamType],
  infos: &[ParamInfo],
  custom_types: Option<&ParameterTypeRegistry>,
) -> Result<Vec<StepParam>> {
  let mut params = Vec::with_capacity(types.len());

  for info in infos {
    let cap = match &info.slot {
      ParamSlot::Positional(index) => captures.get(*index).map(|m| m.as_str()).unwrap_or(""),
      ParamSlot::Named(names) => names
        .iter()
        .find_map(|name| captures.name(name))
        .map(|m| m.as_str())
        .unwrap_or(""),
    };

    let param = match &info.ty {
      ParamType::String => StepParam::String(cap.to_string()),
      ParamType::Int => {
        let val = cap
          .parse::<i64>()
          .map_err(|e| invalid_expr(format!("failed to parse int param \"{cap}\": {e}")))?;
        StepParam::Int(val)
      },
      ParamType::Float => {
        let val = cap
          .parse::<f64>()
          .map_err(|e| invalid_expr(format!("failed to parse float param \"{cap}\": {e}")))?;
        StepParam::Float(val)
      },
      ParamType::Word | ParamType::Anonymous => StepParam::Word(cap.to_string()),
      ParamType::Custom(name) => match custom_types.and_then(|registry| registry.find(name)) {
        Some(custom) => match &custom.transformer {
          Some(transformer) => transformer(cap),
          None => StepParam::Custom {
            type_name: name.clone(),
            value: cap.to_string(),
          },
        },
        None => StepParam::Custom {
          type_name: name.clone(),
          value: cap.to_string(),
        },
      },
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

  fn registry(types: &[(&str, &str)]) -> ParameterTypeRegistry {
    let mut reg = ParameterTypeRegistry::new();
    for (name, regex) in types {
      reg
        .register(crate::param_type::CustomParamType {
          name: (*name).to_string(),
          regex: (*regex).to_string(),
          transformer: None,
        })
        .expect("parameter type should register");
    }
    reg
  }

  fn param_of(expr: &CompiledExpression, text: &str) -> Vec<StepParam> {
    let caps = expr.regex.captures(text).expect("step text should match");
    extract_params(&caps, &expr.param_types, &expr.param_infos).expect("params should extract")
  }

  #[test]
  fn custom_type_with_alternation_matches() {
    let reg = registry(&[("color", "red|green|blue")]);
    let expr = compile_with_custom("I pick {color} paint", &reg).unwrap();
    assert!(expr.regex.is_match("I pick green paint"));
    assert!(expr.regex.is_match("I pick red paint"));
    assert!(!expr.regex.is_match("I pick purple paint"));
    assert_eq!(
      param_of(&expr, "I pick green paint"),
      vec![StepParam::Custom {
        type_name: "color".into(),
        value: "green".into(),
      }]
    );
  }

  #[test]
  fn custom_type_with_metacharacters_matches() {
    let reg = registry(&[("amount", r"\d+"), ("unit", r"[a-z]{2,4}")]);
    let expr = compile_with_custom("I pay {amount} {unit}", &reg).unwrap();
    assert!(expr.regex.is_match("I pay 42 usd"));
    assert_eq!(
      param_of(&expr, "I pay 42 usd"),
      vec![
        StepParam::Custom {
          type_name: "amount".into(),
          value: "42".into(),
        },
        StepParam::Custom {
          type_name: "unit".into(),
          value: "usd".into(),
        },
      ]
    );
  }

  #[test]
  fn custom_type_with_inner_capture_groups_matches() {
    let reg = registry(&[("quoted", "\"([^\"]*)\"|'([^']*)'")]);
    let expr = compile_with_custom("I type {quoted} twice", &reg).unwrap();
    assert!(matches!(expr.param_infos[0].slot, ParamSlot::Named(ref n) if n.len() == 2));
    assert_eq!(
      param_of(&expr, "I type 'hi' twice"),
      vec![StepParam::Custom {
        type_name: "quoted".into(),
        value: "hi".into(),
      }]
    );
  }

  #[test]
  fn custom_type_alongside_builtins_keeps_parameter_order() {
    let reg = registry(&[("color", "red|green|blue")]);
    let expr = compile_with_custom("I paint {string} {color} {int} time(s)", &reg).unwrap();
    assert_eq!(
      param_of(&expr, "I paint \"the door\" blue 3 times"),
      vec![
        StepParam::String("the door".into()),
        StepParam::Custom {
          type_name: "color".into(),
          value: "blue".into(),
        },
        StepParam::Int(3),
      ]
    );
  }

  #[test]
  fn custom_type_transformer_runs_on_the_matched_text() {
    let mut reg = ParameterTypeRegistry::new();
    reg
      .register(crate::param_type::CustomParamType {
        name: "amount".into(),
        regex: r"\d+".into(),
        transformer: Some(std::sync::Arc::new(|s: &str| {
          StepParam::Int(s.parse::<i64>().unwrap_or(-1) * 2)
        })),
      })
      .unwrap();
    let expr = compile_with_custom("I pay {amount}", &reg).unwrap();
    let caps = expr.regex.captures("I pay 21").unwrap();
    let params = extract_params_with_custom(&caps, &expr.param_types, &expr.param_infos, Some(&reg)).unwrap();
    assert_eq!(params, vec![StepParam::Int(42)]);
  }

  #[test]
  fn builtin_and_duplicate_names_are_rejected_at_registration() {
    let mut reg = ParameterTypeRegistry::new();
    let builtin = reg
      .register(crate::param_type::CustomParamType {
        name: "int".into(),
        regex: "one|two".into(),
        transformer: None,
      })
      .unwrap_err()
      .to_string();
    assert!(builtin.contains("built-in parameter type"), "got: {builtin}");

    reg
      .register(crate::param_type::CustomParamType {
        name: "color".into(),
        regex: "red|blue".into(),
        transformer: None,
      })
      .unwrap();
    let dup = reg
      .register(crate::param_type::CustomParamType {
        name: "color".into(),
        regex: "green".into(),
        transformer: None,
      })
      .unwrap_err()
      .to_string();
    assert!(dup.contains("already a parameter type"), "got: {dup}");
  }

  #[test]
  fn unknown_parameter_type_is_a_compile_error() {
    let err = compile("I pick {color} paint").unwrap_err().to_string();
    assert!(err.contains("undefined parameter type {color}"), "got: {err}");
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
