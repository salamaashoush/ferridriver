use super::{StepCategory, StepDef, js_escape, q};

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
  steps.push(Box::new(ScrollTo));
  steps.push(Box::new(ScrollDown));
  steps.push(Box::new(ScrollUp));
}

step!(Click {
    category: StepCategory::Interaction,
    pattern: r"^I click (.+)$",
    description: "Click an element",
    example: "When I click \"#submit\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let el = super::find(page, &sel).await.map_err(|e| format!("'{sel}': {e}"))?;
        el.click().await.map_err(|e| e.clone())?;
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
        let el = super::find(page, &sel).await.map_err(|e| format!("'{sel}': {e}"))?;
        el.dblclick().await.map_err(|e| e.clone())?;
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
        page.click_at(x, y).await.map_err(|e| e.clone())?;
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
        let el = super::find(page, &sel).await.map_err(|e| format!("'{sel}': {e}"))?;
        el.hover().await.map_err(|e| e.clone())?;
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
        let el = super::find(page, &sel).await.map_err(|e| format!("'{sel}': {e}"))?;
        crate::actions::fill(&el, &val).await?;
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
                let el = super::find(page, sel).await.map_err(|e| format!("'{sel}': {e}"))?;
                crate::actions::fill(&el, val).await?;
            }
        }
        Ok(None)
    }
});

step!(Clear {
    category: StepCategory::Interaction,
    pattern: r"^I clear (.+)$",
    description: "Clear an input field (dispatches input and change events)",
    example: "When I clear \"#search\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let el = super::find(page, &sel).await.map_err(|e| format!("'{sel}': {e}"))?;
        el.call_js_fn("function() { \
            this.focus(); \
            this.value = ''; \
            this.dispatchEvent(new Event('input', {bubbles: true})); \
            this.dispatchEvent(new Event('change', {bubbles: true})); \
        }").await.map_err(|e| e.clone())?;
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
        crate::actions::select_option(&el, page, &val).await?;
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
        page.type_str(&text).await.map_err(|e| e.clone())?;
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
        page.press_key(&key).await.map_err(|e| e.clone())?;
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
        page.evaluate(&format!("document.querySelector('{}')?.focus()", js_escape(&sel)))
            .await.map_err(|e| e.clone())?;
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
        let el = super::find(page, &sel).await.map_err(|e| e.clone())?;
        el.scroll_into_view().await.map_err(|e| e.clone())?;
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
        page.evaluate(&format!("window.scrollBy(0, {px})")).await.map_err(|e| e.clone())?;
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
        page.evaluate(&format!("window.scrollBy(0, -{px})")).await.map_err(|e| e.clone())?;
        Ok(None)
    }
});
