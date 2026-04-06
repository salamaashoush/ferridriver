//! Step registry: collects step definitions from inventory + runtime registration.

use crate::expression;
use crate::filter::TagExpression;
use crate::hook::{Hook, HookHandler, HookRegistration, HookRegistry};
use crate::step::{
  MatchError, StepDef, StepHandler, StepLocation, StepMatch, StepParam, StepRegistration,
};

/// Central registry of step definitions and hooks.
pub struct StepRegistry {
  steps: Vec<StepDef>,
  hooks: HookRegistry,
}

impl StepRegistry {
  /// Build the registry from inventory-collected registrations.
  pub fn build() -> Self {
    let mut registry = Self {
      steps: Vec::new(),
      hooks: HookRegistry::new(),
    };

    // Collect step registrations from #[given], #[when], #[then], #[step] macros.
    for reg in inventory::iter::<StepRegistration> {
      match expression::compile(reg.expression) {
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
        }
        Err(e) => {
          tracing::error!(
            "failed to compile step expression \"{}\" at {}:{}: {}",
            reg.expression,
            reg.file,
            reg.line,
            e
          );
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
    let compiled = expression::compile(expr)?;
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
    let mut matches: Vec<(&StepDef, Vec<StepParam>)> = Vec::new();

    for def in &self.steps {
      if let Some(captures) = def.regex.captures(text) {
        match expression::extract_params(&captures, &def.param_types, &def.param_infos) {
          Ok(params) => matches.push((def, params)),
          Err(_) => continue,
        }
      }
    }

    match matches.len() {
      0 => Err(MatchError::Undefined {
        text: text.to_string(),
        suggestions: self.suggest(text),
      }),
      1 => {
        let (def, params) = matches.into_iter().next().expect("checked len == 1");
        Ok(StepMatch { def, params })
      }
      _ => Err(MatchError::Ambiguous {
        text: text.to_string(),
        matches: matches.iter().map(|(def, _)| def.location.clone()).collect(),
      }),
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
