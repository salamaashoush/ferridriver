use super::{q, js_escape, StepCategory, StepDef};

pub fn register(steps: &mut Vec<Box<dyn StepDef>>) {
    steps.push(Box::new(SetLocalStorage));
    steps.push(Box::new(RemoveLocalStorage));
    steps.push(Box::new(ClearLocalStorage));
}

step!(SetLocalStorage {
    category: StepCategory::Storage,
    pattern: r#"^I set localStorage (.+) to (.+)$"#,
    description: "Set localStorage item",
    example: "When I set localStorage \"key\" to \"value\"",
    execute(page, caps, _table, _vars) {
        let key = q(&caps[1]);
        let val = q(&caps[2]);
        page.evaluate(format!("localStorage.setItem('{}', '{}')", js_escape(&key), js_escape(&val)))
            .await.map_err(|e| format!("{e}"))?;
        Ok(None)
    }
});

step!(RemoveLocalStorage {
    category: StepCategory::Storage,
    pattern: r#"^I remove localStorage (.+)$"#,
    description: "Remove localStorage item",
    example: "When I remove localStorage \"key\"",
    execute(page, caps, _table, _vars) {
        let key = q(&caps[1]);
        page.evaluate(format!("localStorage.removeItem('{}')", js_escape(&key)))
            .await.map_err(|e| format!("{e}"))?;
        Ok(None)
    }
});

step!(ClearLocalStorage {
    category: StepCategory::Storage,
    pattern: r#"^I clear localStorage$"#,
    description: "Clear all localStorage",
    example: "When I clear localStorage",
    execute(page, _caps, _table, _vars) {
        page.evaluate("localStorage.clear()").await.map_err(|e| format!("{e}"))?;
        Ok(None)
    }
});
