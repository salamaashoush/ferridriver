use super::{StepCategory, StepDef, q};

pub fn register(steps: &mut Vec<Box<dyn StepDef>>) {
  steps.push(Box::new(NavigateNoWait));
  steps.push(Box::new(Navigate));
  steps.push(Box::new(GoBack));
  steps.push(Box::new(GoForward));
  steps.push(Box::new(Reload));
}

step!(NavigateNoWait {
    category: StepCategory::Navigation,
    pattern: r"^I navigate to (.+) without waiting$",
    description: "Navigate without waiting for page load",
    example: "When I navigate to \"https://example.com\" without waiting",
    execute(page, caps, _table, _vars) {
        let url = q(&caps[1]);
        // Use window.location to start navigation without waiting for load.
        // page.goto() waits for loadEventFired -- this returns immediately.
        page.evaluate(&format!("window.location.href = '{}'", super::js_escape(&url)))
            .await.map_err(|e| format!("Navigate: {e}"))?;
        Ok(None)
    }
});

step!(Navigate {
    category: StepCategory::Navigation,
    pattern: r"^I navigate to (.+)$",
    description: "Navigate to URL and wait for load",
    example: "Given I navigate to \"https://example.com\"",
    execute(page, caps, _table, _vars) {
        let url = q(&caps[1]);
        // goto() already waits for page load internally
        page.goto(&url).await.map_err(|e| format!("Navigate: {e}"))?;
        Ok(None)
    }
});

step!(GoBack {
    category: StepCategory::Navigation,
    pattern: r"^I go back$",
    description: "Go back in history",
    example: "When I go back",
    execute(page, _caps, _table, _vars) {
        page.go_back().await.map_err(|e| e.clone())?;
        Ok(None)
    }
});

step!(GoForward {
    category: StepCategory::Navigation,
    pattern: r"^I go forward$",
    description: "Go forward in history",
    example: "When I go forward",
    execute(page, _caps, _table, _vars) {
        page.go_forward().await.map_err(|e| e.clone())?;
        Ok(None)
    }
});

step!(Reload {
    category: StepCategory::Navigation,
    pattern: r"^I reload(?: the page)?$",
    description: "Reload the page",
    example: "When I reload the page",
    execute(page, _caps, _table, _vars) {
        page.reload().await.map_err(|e| e.clone())?;
        Ok(None)
    }
});
