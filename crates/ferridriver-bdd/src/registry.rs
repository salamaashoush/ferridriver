//! Step registry: collects step definitions from inventory + runtime registration.

use std::sync::Arc;

use crate::expression;
use crate::filter::TagExpression;
use crate::hook::{Hook, HookHandler, HookRegistration, HookRegistry};
use crate::param_type::{CustomParamType, ParameterTypeRegistration, ParameterTypeRegistry};
use crate::step::{MatchError, StepDef, StepHandler, StepLocation, StepMatch, StepParam, StepRegistration};

/// Central registry of step definitions and hooks.
pub struct StepRegistry {
  steps: Vec<StepDef>,
  hooks: HookRegistry,
  param_types: ParameterTypeRegistry,
}

impl StepRegistry {
  /// Build the registry from inventory-collected registrations.
  pub fn build() -> Self {
    let mut registry = Self {
      steps: Vec::new(),
      hooks: HookRegistry::new(),
      param_types: ParameterTypeRegistry::new(),
    };

    // Collect custom parameter type registrations from inventory.
    for reg in inventory::iter::<ParameterTypeRegistration> {
      let transformer = reg.transformer_factory.map(|f| f());
      registry.param_types.register(CustomParamType {
        name: reg.name.to_string(),
        regex: reg.regex.to_string(),
        transformer,
      });
    }

    // Collect step registrations from #[given], #[when], #[then], #[step] macros.
    for reg in inventory::iter::<StepRegistration> {
      if reg.is_regex {
        // Raw regex step: compile directly, all params are String.
        match regex::Regex::new(reg.expression) {
          Ok(regex) => {
            let num_groups = regex.captures_len().saturating_sub(1);
            registry.steps.push(StepDef {
              kind: reg.kind,
              expression: reg.expression.to_string(),
              regex,
              param_types: vec![expression::ParamType::Word; num_groups],
              param_infos: (0..num_groups)
                .map(|i| expression::ParamInfo {
                  ty: expression::ParamType::Word,
                  id: i,
                })
                .collect(),
              handler: (reg.handler_factory)(),
              location: StepLocation {
                file: reg.file,
                line: reg.line,
              },
            });
          },
          Err(e) => {
            tracing::error!(
              "failed to compile regex step \"{}\" at {}:{}: {}",
              reg.expression,
              reg.file,
              reg.line,
              e
            );
          },
        }
      } else {
        // Cucumber expression: compile via expression engine.
        match expression::compile_with_custom(reg.expression, &registry.param_types) {
          Ok(compiled) => {
            registry.steps.push(StepDef {
              kind: reg.kind,
              expression: reg.expression.to_string(),
              regex: compiled.regex,
              param_types: compiled.param_types,
              param_infos: compiled.param_infos,
              handler: (reg.handler_factory)(),
              location: StepLocation {
                file: reg.file,
                line: reg.line,
              },
            });
          },
          Err(e) => {
            tracing::error!(
              "failed to compile step expression \"{}\" at {}:{}: {}",
              reg.expression,
              reg.file,
              reg.line,
              e
            );
          },
        }
      }
    }

    // Collect hook registrations from #[before], #[after] macros.
    for reg in inventory::iter::<HookRegistration> {
      let tag_filter = reg.tag_filter.as_ref().and_then(|s| {
        TagExpression::parse(s)
          .map_err(|e| {
            tracing::error!(
              "failed to parse hook tag filter \"{}\" at {}:{}: {}",
              s,
              reg.file,
              reg.line,
              e
            );
          })
          .ok()
      });

      registry.hooks.register(Hook {
        point: reg.point,
        tag_filter,
        order: reg.order,
        handler: (reg.handler_factory)(),
        location: StepLocation {
          file: reg.file,
          line: reg.line,
        },
      });
    }

    registry
  }

  /// Register a step definition at runtime (for NAPI/TS step registration).
  pub fn register_step(&mut self, def: StepDef) {
    self.steps.push(def);
  }

  /// Register a step from an expression string and handler.
  pub fn register(
    &mut self,
    kind: crate::step::StepKind,
    expr: &str,
    handler: StepHandler,
    location: StepLocation,
  ) -> Result<(), String> {
    let compiled = expression::compile_with_custom(expr, &self.param_types)?;
    self.steps.push(StepDef {
      kind,
      expression: expr.to_string(),
      regex: compiled.regex,
      param_types: compiled.param_types,
      param_infos: compiled.param_infos,
      handler,
      location,
    });
    Ok(())
  }

  /// Register a step from a raw regex pattern (not a Cucumber expression).
  /// All capture groups are extracted as string params.
  pub fn register_regex(
    &mut self,
    kind: crate::step::StepKind,
    pattern: &str,
    handler: StepHandler,
    location: StepLocation,
  ) -> Result<(), String> {
    let regex = regex::Regex::new(pattern).map_err(|e| format!("invalid regex \"{pattern}\": {e}"))?;
    let num_groups = regex.captures_len().saturating_sub(1);
    self.steps.push(StepDef {
      kind,
      expression: pattern.to_string(),
      regex,
      param_types: vec![expression::ParamType::Word; num_groups],
      param_infos: (0..num_groups)
        .map(|i| expression::ParamInfo {
          ty: expression::ParamType::Word,
          id: i,
        })
        .collect(),
      handler,
      location,
    });
    Ok(())
  }

  /// Register a custom parameter type for Cucumber expressions.
  pub fn register_param_type(&mut self, param_type: CustomParamType) {
    self.param_types.register(param_type);
  }

  /// Access the hook registry.
  pub fn hooks(&self) -> &HookRegistry {
    &self.hooks
  }

  /// Mutable access to the hook registry.
  pub fn hooks_mut(&mut self) -> &mut HookRegistry {
    &mut self.hooks
  }

  /// Find the matching step definition for a given step text.
  ///
  /// Matching is keyword-agnostic per the Cucumber specification:
  /// a Given step definition can match a When step line and vice versa.
  ///
  /// Returns `Ambiguous` if multiple definitions match.
  /// Returns `Undefined` with suggestions if no definition matches.
  pub fn find_match(&self, text: &str) -> Result<StepMatch<'_>, MatchError> {
    // Fast path: find first match, then only continue scanning to detect ambiguity.
    // Most steps have exactly 1 match among 100+ definitions, so we avoid
    // collecting all matches into a Vec on the common path.
    let mut first_match: Option<(&StepDef, Vec<StepParam>)> = None;

    for def in &self.steps {
      if let Some(captures) = def.regex.captures(text) {
        match expression::extract_params_with_custom(
          &captures,
          &def.param_types,
          &def.param_infos,
          Some(&self.param_types),
        ) {
          Ok(params) => {
            if let Some((first_def, _)) = &first_match {
              // Second match found -- ambiguous.
              // Collect remaining matches for the error message.
              let mut all = vec![(first_def.location.clone(), first_def.expression.clone())];
              all.push((def.location.clone(), def.expression.clone()));
              for remaining in self
                .steps
                .iter()
                .skip(self.steps.iter().position(|d| std::ptr::eq(d, def)).unwrap_or(0) + 1)
              {
                if let Some(caps) = remaining.regex.captures(text) {
                  if expression::extract_params_with_custom(
                    &caps,
                    &remaining.param_types,
                    &remaining.param_infos,
                    Some(&self.param_types),
                  )
                  .is_ok()
                  {
                    all.push((remaining.location.clone(), remaining.expression.clone()));
                  }
                }
              }
              tracing::warn!(target: "ferridriver::bdd::step", text, count = all.len(), "step AMBIGUOUS");
              return Err(MatchError::Ambiguous {
                text: text.to_string(),
                matches: all.iter().map(|(loc, _)| loc.clone()).collect(),
                expressions: all.iter().map(|(_, expr)| expr.clone()).collect(),
              });
            }
            first_match = Some((def, params));
          },
          Err(_) => continue,
        }
      }
    }

    match first_match {
      Some((def, params)) => {
        tracing::debug!(target: "ferridriver::bdd::step", text, expression = def.expression, "step matched");
        Ok(StepMatch { def, params })
      },
      None => {
        tracing::debug!(target: "ferridriver::bdd::step", text, "step UNDEFINED — no match");
        Err(MatchError::Undefined {
          text: text.to_string(),
          suggestions: self.suggest(text),
        })
      },
    }
  }

  /// List all registered step definitions.
  pub fn steps(&self) -> &[StepDef] {
    &self.steps
  }

  /// Generate suggestions for an undefined step (simple substring matching).
  fn suggest(&self, text: &str) -> Vec<String> {
    let text_lower = text.to_lowercase();
    let words: Vec<&str> = text_lower.split_whitespace().collect();

    let mut scored: Vec<(usize, &str)> = self
      .steps
      .iter()
      .map(|def| {
        let expr_lower = def.expression.to_lowercase();
        let score = words.iter().filter(|w| expr_lower.contains(**w)).count();
        (score, def.expression.as_str())
      })
      .filter(|(score, _)| *score > 0)
      .collect();

    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.truncate(5);
    scored.into_iter().map(|(_, expr)| expr.to_string()).collect()
  }

  /// Generate a markdown reference of all step definitions grouped by kind.
  pub fn reference(&self) -> String {
    let mut output = String::from("# Step Reference\n\n");

    for kind in &[
      crate::step::StepKind::Given,
      crate::step::StepKind::When,
      crate::step::StepKind::Then,
      crate::step::StepKind::Step,
    ] {
      let steps: Vec<&StepDef> = self.steps.iter().filter(|s| s.kind == *kind).collect();
      if steps.is_empty() {
        continue;
      }

      output.push_str(&format!("## {kind}\n\n"));
      for step in steps {
        output.push_str(&format!("- `{}`\n", step.expression));
      }
      output.push('\n');
    }

    output
  }
}
