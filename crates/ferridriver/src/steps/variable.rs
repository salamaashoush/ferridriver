use super::{StepCategory, StepDef, q};

pub fn register(steps: &mut Vec<Box<dyn StepDef>>) {
  steps.push(Box::new(StoreText));
  steps.push(Box::new(StoreValue));
  steps.push(Box::new(StoreAttr));
  steps.push(Box::new(StoreUrl));
  steps.push(Box::new(StoreTitle));
  steps.push(Box::new(EvalAndStore));
  steps.push(Box::new(SetVar));
}

step!(StoreText {
    category: StepCategory::Variable,
    pattern: r"^I store the text of (.+) as \$(\w+)$",
    description: "Store element text in variable",
    example: "When I store the text of \"h1\" as $title",
    execute(page, caps, _table, vars) {
        let sel = q(&caps[1]);
        let var = caps[2].to_string();
        let loc = page.locator(&sel, None);
        let val = loc.inner_text().await.unwrap_or_default();
        vars.insert(var, val);
        Ok(None)
    }
});

step!(StoreValue {
    category: StepCategory::Variable,
    pattern: r"^I store the value of (.+) as \$(\w+)$",
    description: "Store input value in variable",
    example: "When I store the value of \"#email\" as $email",
    execute(page, caps, _table, vars) {
        let sel = q(&caps[1]);
        let var = caps[2].to_string();
        let loc = page.locator(&sel, None);
        let val = loc.input_value().await.unwrap_or_default();
        vars.insert(var, val);
        Ok(None)
    }
});

step!(StoreAttr {
    category: StepCategory::Variable,
    pattern: r"^I store the attribute (.+) of (.+) as \$(\w+)$",
    description: "Store element attribute in variable",
    example: "When I store the attribute \"href\" of \"a\" as $link",
    execute(page, caps, _table, vars) {
        let attr = q(&caps[1]);
        let sel = q(&caps[2]);
        let var = caps[3].to_string();
        let loc = page.locator(&sel, None);
        let val = loc.get_attribute(&attr).await?.unwrap_or_default();
        vars.insert(var, val);
        Ok(None)
    }
});

step!(StoreUrl {
    category: StepCategory::Variable,
    pattern: r"^I store the URL as \$(\w+)$",
    description: "Store current URL in variable",
    example: "When I store the URL as $url",
    execute(page, caps, _table, vars) {
        let var = caps[1].to_string();
        let url = page.url().await.unwrap_or_default();
        vars.insert(var, url);
        Ok(None)
    }
});

step!(StoreTitle {
    category: StepCategory::Variable,
    pattern: r"^I store the title as \$(\w+)$",
    description: "Store page title in variable",
    example: "When I store the title as $title",
    execute(page, caps, _table, vars) {
        let var = caps[1].to_string();
        let title = page.title().await.unwrap_or_default();
        vars.insert(var, title);
        Ok(None)
    }
});

step!(EvalAndStore {
    category: StepCategory::Variable,
    pattern: r"^I evaluate (.+) and store as \$(\w+)$",
    description: "Evaluate JS and store result",
    example: "When I evaluate \"document.title\" and store as $t",
    execute(page, caps, _table, vars) {
        let expr = q(&caps[1]);
        let var = caps[2].to_string();
        let r = page.inner().evaluate(expr.as_str()).await?;
        let val = r
            .map(|v| v.to_string().trim_matches('"').to_string())
            .unwrap_or_default();
        vars.insert(var, val);
        Ok(None)
    }
});

step!(SetVar {
    category: StepCategory::Variable,
    pattern: r"^I set \$(\w+) to (.+)$",
    description: "Set a variable to a value",
    example: "Given I set $name to \"Alice\"",
    execute(_page, caps, _table, vars) {
        let var = caps[1].to_string();
        let val = q(&caps[2]);
        vars.insert(var, val);
        Ok(None)
    }
});
