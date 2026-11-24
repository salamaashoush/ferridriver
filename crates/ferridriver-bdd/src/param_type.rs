//! Custom parameter type registry for extending Cucumber expressions.

use std::sync::Arc;

use rustc_hash::FxHashMap;

use crate::step::StepParam;

/// Converts a matched parameter's text to its value.
pub type ParamTransformer = Arc<dyn Fn(&str) -> StepParam + Send + Sync>;

/// A custom parameter type definition.
pub struct CustomParamType {
  /// The parameter name used in expressions (e.g., "color").
  pub name: String,
  /// Regex pattern to match (e.g., "red|green|blue").
  pub regex: String,
  /// Optional transformer function that converts the matched text to a `StepParam`.
  pub transformer: Option<ParamTransformer>,
}

/// Registry of custom parameter types.
pub struct ParameterTypeRegistry {
  types: FxHashMap<String, CustomParamType>,
}

impl ParameterTypeRegistry {
  #[must_use]
  pub fn new() -> Self {
    Self {
      types: FxHashMap::default(),
    }
  }

  /// Register a custom parameter type.
  ///
  /// Mirrors cucumber-js' `ParameterTypeRegistry`: neither a built-in name nor
  /// an already-registered one can be taken over, because the expression
  /// compiler resolves built-ins itself and would otherwise ignore the
  /// definition without saying so.
  ///
  /// # Errors
  ///
  /// When the name is a built-in parameter type or is already registered.
  pub fn register(&mut self, param_type: CustomParamType) -> ferridriver::error::Result<()> {
    if crate::expression::BUILTIN_PARAM_TYPES.contains(&param_type.name.as_str()) {
      return Err(ferridriver::FerriError::invalid_argument(
        "parameter-type",
        format!(
          "{{{}}} is a built-in parameter type and cannot be redefined",
          param_type.name
        ),
      ));
    }
    if self.types.contains_key(&param_type.name) {
      return Err(ferridriver::FerriError::invalid_argument(
        "parameter-type",
        format!("there is already a parameter type with name {}", param_type.name),
      ));
    }
    self.types.insert(param_type.name.clone(), param_type);
    Ok(())
  }

  #[must_use]
  pub fn find(&self, name: &str) -> Option<&CustomParamType> {
    self.types.get(name)
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.types.is_empty()
  }
}

impl Default for ParameterTypeRegistry {
  fn default() -> Self {
    Self::new()
  }
}

/// What proc macros submit via inventory for custom parameter types.
pub struct ParameterTypeRegistration {
  pub name: &'static str,
  pub regex: &'static str,
  pub transformer_factory: Option<fn() -> ParamTransformer>,
}

inventory::collect!(ParameterTypeRegistration);
