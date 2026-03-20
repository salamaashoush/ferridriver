//! Central step registry — compiled once, dispatches step execution.

use super::{StepCategory, StepDef};
use std::sync::OnceLock;

/// Holds all registered steps in match-priority order.
pub struct StepRegistry {
    steps: Vec<Box<dyn StepDef>>,
}

impl StepRegistry {
    /// Build the registry. Each category module pushes its steps.
    fn build() -> Self {
        let mut steps: Vec<Box<dyn StepDef>> = Vec::new();

        // Registration order = match priority (more specific patterns first within a module).
        super::navigation::register(&mut steps);
        super::interaction::register(&mut steps);
        super::wait::register(&mut steps);
        super::assertion::register(&mut steps);
        super::variable::register(&mut steps);
        super::cookie::register(&mut steps);
        super::storage::register(&mut steps);
        super::screenshot::register(&mut steps);
        super::javascript::register(&mut steps);

        Self { steps }
    }

    /// Global singleton — patterns compiled exactly once.
    pub fn global() -> &'static Self {
        static INSTANCE: OnceLock<StepRegistry> = OnceLock::new();
        INSTANCE.get_or_init(Self::build)
    }

    /// Find the first matching step and execute it.
    pub async fn execute(
        &self,
        page: &chromiumoxide::Page,
        body: &str,
        data_table: Option<&[Vec<String>]>,
        vars: &mut std::collections::HashMap<String, String>,
    ) -> Result<Option<serde_json::Value>, String> {
        for step in &self.steps {
            if let Some(caps) = step.pattern().captures(body) {
                return step.execute(page, &caps, data_table, vars).await;
            }
        }
        // Build helpful error with closest matches
        let mut suggestions = Vec::new();
        let body_lower = body.to_lowercase();
        for step in &self.steps {
            let desc_lower = step.description().to_lowercase();
            if body_lower.split_whitespace().any(|w| desc_lower.contains(w)) {
                suggestions.push(format!("  - {}", step.example()));
                if suggestions.len() >= 3 {
                    break;
                }
            }
        }
        let hint = if suggestions.is_empty() {
            String::new()
        } else {
            format!("\n\nDid you mean:\n{}", suggestions.join("\n"))
        };
        Err(format!("Unknown step: '{body}'{hint}"))
    }

    /// Auto-generate step reference from registered steps.
    pub fn reference(&self) -> String {
        let mut out = String::new();
        let mut current_cat: Option<StepCategory> = None;

        for step in &self.steps {
            let cat = step.category();
            if current_cat != Some(cat) {
                current_cat = Some(cat);
                out.push_str(&format!("\n## {:?}\n", cat));
            }
            out.push_str(&format!("- {} — `{}`\n", step.description(), step.example()));
        }
        out
    }

    /// List all steps as structured data.
    pub fn list(&self) -> Vec<StepInfo> {
        self.steps
            .iter()
            .map(|s| StepInfo {
                category: format!("{:?}", s.category()),
                description: s.description().to_string(),
                example: s.example().to_string(),
            })
            .collect()
    }
}

#[derive(Debug, serde::Serialize)]
pub struct StepInfo {
    pub category: String,
    pub description: String,
    pub example: String,
}
