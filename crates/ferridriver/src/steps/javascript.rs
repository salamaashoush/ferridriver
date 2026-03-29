use super::{StepCategory, StepDef, q};

pub fn register(steps: &mut Vec<Box<dyn StepDef>>) {
  steps.push(Box::new(Evaluate));
}

step!(Evaluate {
    category: StepCategory::JavaScript,
    pattern: r"^I evaluate (.+)$",
    description: "Execute JavaScript and return the result",
    example: "When I evaluate \"document.title\"",
    execute(page, caps, _table, _vars) {
        let expr = q(&caps[1]);
        let result = page.evaluate(expr.as_str()).await?;
        Ok(result)
    }
});
