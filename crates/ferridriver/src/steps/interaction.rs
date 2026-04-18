use super::{StepCategory, StepDef, q};

pub fn register(steps: &mut Vec<Box<dyn StepDef>>) {
  steps.push(Box::new(DoubleClick));
  steps.push(Box::new(ClickAt));
  steps.push(Box::new(Click));
  steps.push(Box::new(Hover));
  steps.push(Box::new(FillForm));
  steps.push(Box::new(Fill));
  steps.push(Box::new(Clear));
  steps.push(Box::new(SelectOption));
  steps.push(Box::new(TypeText));
  steps.push(Box::new(PressKey));
  steps.push(Box::new(Focus));
  steps.push(Box::new(Blur));
  steps.push(Box::new(ScrollTo));
  steps.push(Box::new(ScrollDown));
  steps.push(Box::new(ScrollUp));
  steps.push(Box::new(Check));
  steps.push(Box::new(Uncheck));
}

step!(Click {
    category: StepCategory::Interaction,
    pattern: r"^I click (.+)$",
    description: "Click an element",
    example: "When I click \"#submit\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let loc = page.locator(&sel, None);
        loc.click(None).await?;
        Ok(None)
    }
});

step!(DoubleClick {
    category: StepCategory::Interaction,
    pattern: r"^I double[- ]click (.+)$",
    description: "Double-click an element (fires dblclick event)",
    example: "When I double-click \"#item\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let loc = page.locator(&sel, None);
        loc.dblclick(None).await?;
        Ok(None)
    }
});

step!(ClickAt {
    category: StepCategory::Interaction,
    pattern: r"^I click at (?:coordinates )?(\d+)\s*,\s*(\d+)$",
    description: "Click at X,Y coordinates",
    example: "When I click at 100, 200",
    execute(page, caps, _table, _vars) {
        let x: f64 = caps[1].parse().map_err(|_| "Invalid x")?;
        let y: f64 = caps[2].parse().map_err(|_| "Invalid y")?;
        page.click_at(x, y).await.map_err(|e| e.to_string())?;
        Ok(None)
    }
});

step!(Hover {
    category: StepCategory::Interaction,
    pattern: r"^I hover (?:over )?(.+)$",
    description: "Hover over an element",
    example: "When I hover over \"#menu\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let loc = page.locator(&sel, None);
        loc.hover(None).await?;
        Ok(None)
    }
});

step!(Fill {
    category: StepCategory::Interaction,
    pattern: r"^I fill (.+) with (.+)$",
    description: "Clear and type into an input",
    example: "When I fill \"#email\" with \"test@example.com\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let val = q(&caps[2]);
        let loc = page.locator(&sel, None);
        loc.fill(&val, None).await?;
        Ok(None)
    }
});

step!(FillForm {
    category: StepCategory::Interaction,
    pattern: r"^I fill the form:?$",
    description: "Fill multiple form fields via data table",
    example: "When I fill the form:\n  | #name | Alice |\n  | #email | alice@test.com |",
    execute(page, _caps, table, _vars) {
        let rows = table.ok_or("'I fill the form' requires a data table")?;
        for row in rows {
            if row.len() >= 2 {
                let sel = &row[0];
                let val = &row[1];
                let loc = page.locator(sel, None);
                loc.fill(val, None).await?;
            }
        }
        Ok(None)
    }
});

step!(Clear {
    category: StepCategory::Interaction,
    pattern: r"^I clear (.+)$",
    description: "Clear an input field",
    example: "When I clear \"#search\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let loc = page.locator(&sel, None);
        loc.fill("", None).await?;
        Ok(None)
    }
});

step!(SelectOption {
    category: StepCategory::Interaction,
    pattern: r"^I select (.+) from (.+)$",
    description: "Select dropdown option",
    example: "When I select \"admin\" from \"#role\"",
    execute(page, caps, _table, _vars) {
        let val = q(&caps[1]);
        let sel = q(&caps[2]);
        let el = super::find(page, &sel).await.map_err(|e| format!("'{sel}': {e}"))?;
        crate::actions::select_option(&el, page.inner(), &val).await?;
        Ok(None)
    }
});

step!(TypeText {
    category: StepCategory::Interaction,
    pattern: r"^I type (.+)$",
    description: "Type text into focused element",
    example: "When I type \"hello world\"",
    execute(page, caps, _table, _vars) {
        let text = q(&caps[1]);
        page.keyboard().r#type(&text).await.map_err(|e| e.to_string())?;
        Ok(None)
    }
});

step!(PressKey {
    category: StepCategory::Interaction,
    pattern: r"^I press (?:key )?(.+)$",
    description: "Press a key or combo",
    example: "When I press \"Enter\"",
    execute(page, caps, _table, _vars) {
        let key = q(&caps[1]);
        page.keyboard().press(&key).await.map_err(|e| e.to_string())?;
        Ok(None)
    }
});

step!(Focus {
    category: StepCategory::Interaction,
    pattern: r"^I focus (.+)$",
    description: "Focus an element",
    example: "When I focus \"#input\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let loc = page.locator(&sel, None);
        loc.focus().await?;
        Ok(None)
    }
});

step!(Blur {
    category: StepCategory::Interaction,
    pattern: r"^I blur (.+)$",
    description: "Blur (unfocus) an element",
    example: "When I blur \"#input\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let loc = page.locator(&sel, None);
        loc.blur().await?;
        Ok(None)
    }
});

step!(Check {
    category: StepCategory::Interaction,
    pattern: r"^I check (.+)$",
    description: "Check a checkbox",
    example: "When I check \"#agree\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let loc = page.locator(&sel, None);
        loc.check(None).await?;
        Ok(None)
    }
});

step!(Uncheck {
    category: StepCategory::Interaction,
    pattern: r"^I uncheck (.+)$",
    description: "Uncheck a checkbox",
    example: "When I uncheck \"#agree\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let loc = page.locator(&sel, None);
        loc.uncheck(None).await?;
        Ok(None)
    }
});

step!(ScrollTo {
    category: StepCategory::Interaction,
    pattern: r"^I scroll to (.+)$",
    description: "Scroll element into view",
    example: "When I scroll to \"#footer\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let el = super::find(page, &sel).await?;
        el.scroll_into_view().await?;
        Ok(None)
    }
});

step!(ScrollDown {
    category: StepCategory::Interaction,
    pattern: r"^I scroll down(?: by (\d+))?$",
    description: "Scroll down",
    example: "When I scroll down by 300",
    execute(page, caps, _table, _vars) {
        let px = caps.get(1).and_then(|m| m.as_str().parse::<f64>().ok()).unwrap_or(300.0);
        page.mouse().wheel(0.0, px).await.map_err(|e| e.to_string())?;
        Ok(None)
    }
});

step!(ScrollUp {
    category: StepCategory::Interaction,
    pattern: r"^I scroll up(?: by (\d+))?$",
    description: "Scroll up",
    example: "When I scroll up by 300",
    execute(page, caps, _table, _vars) {
        let px = caps.get(1).and_then(|m| m.as_str().parse::<f64>().ok()).unwrap_or(300.0);
        page.mouse().wheel(0.0, -px).await.map_err(|e| e.to_string())?;
        Ok(None)
    }
});
