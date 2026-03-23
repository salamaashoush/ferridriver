//! Central step registry — compiled once, dispatches step execution.

use super::{StepCategory, StepDef};
use crate::backend::AnyPage;
use std::sync::OnceLock;

pub struct StepRegistry {
    steps: Vec<Box<dyn StepDef>>,
}

impl StepRegistry {
    fn build() -> Self {
        let mut steps: Vec<Box<dyn StepDef>> = Vec::new();

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

    pub fn global() -> &'static Self {
        static INSTANCE: OnceLock<StepRegistry> = OnceLock::new();
        INSTANCE.get_or_init(Self::build)
    }

    pub async fn execute(
        &self,
        page: &AnyPage,
        body: &str,
        data_table: Option<&[Vec<String>]>,
        vars: &mut std::collections::HashMap<String, String>,
    ) -> Result<Option<serde_json::Value>, String> {
        for step in &self.steps {
            if let Some(caps) = step.pattern().captures(body) {
                return step.execute(page, &caps, data_table, vars).await;
            }
        }
        let mut suggestions = Vec::new();
        let body_lower = body.to_lowercase();
        for step in &self.steps {
            let desc_lower = step.description().to_lowercase();
            if body_lower
                .split_whitespace()
                .any(|w| desc_lower.contains(w))
            {
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

    pub fn reference(&self) -> String {
        let mut out = String::new();
        let mut current_cat: Option<StepCategory> = None;

        for step in &self.steps {
            let cat = step.category();
            if current_cat != Some(cat) {
                current_cat = Some(cat);
                out.push_str(&format!("\n## {:?}\n", cat));
            }
            out.push_str(&format!(
                "- {} — `{}`\n",
                step.description(),
                step.example()
            ));
        }
        out
    }

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
