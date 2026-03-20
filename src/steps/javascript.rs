use super::{q, StepCategory, StepDef};

pub fn register(steps: &mut Vec<Box<dyn StepDef>>) {
    steps.push(Box::new(Evaluate));
}

step!(Evaluate {
    category: StepCategory::JavaScript,
    pattern: r#"^I evaluate (.+)$"#,
    description: "Execute JavaScript",
    example: "When I evaluate \"document.title\"",
    execute(page, caps, _table, _vars) {
        let expr = q(&caps[1]);
        page.evaluate(expr.as_str()).await.map_err(|e| format!("{e}"))?;
        Ok(None)
    }
});
