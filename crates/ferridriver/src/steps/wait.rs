use super::{StepCategory, StepDef, q};

pub fn register(steps: &mut Vec<Box<dyn StepDef>>) {
  // Text variants MUST come before selector variants.
  steps.push(Box::new(WaitTextTimeout));
  steps.push(Box::new(WaitText));
  steps.push(Box::new(WaitSelectorTimeout));
  steps.push(Box::new(WaitSelector));
  steps.push(Box::new(WaitMs));
  steps.push(Box::new(WaitNavigation));
}

step!(WaitSelectorTimeout {
    category: StepCategory::Wait,
    pattern: r"^I wait for (.+?) for (\d+)\s*ms$",
    description: "Wait for selector with timeout",
    example: "When I wait for \"#loading\" for 5000ms",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let ms: u64 = caps[2].parse().unwrap_or(30000);
        wait_for_selector(page, &sel, ms).await?;
        Ok(None)
    }
});

step!(WaitSelector {
    category: StepCategory::Wait,
    pattern: r"^I wait for selector (.+)$",
    description: "Wait for selector to appear",
    example: "When I wait for selector \"#content\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        wait_for_selector(page, &sel, 30000).await?;
        Ok(None)
    }
});

step!(WaitTextTimeout {
    category: StepCategory::Wait,
    pattern: r"^I wait for text (.+?) for (\d+)\s*ms$",
    description: "Wait for text with timeout",
    example: "When I wait for text \"Success\" for 5000ms",
    execute(page, caps, _table, _vars) {
        let text = q(&caps[1]);
        let ms: u64 = caps[2].parse().unwrap_or(30000);
        wait_for_text(page, &text, ms).await?;
        Ok(None)
    }
});

step!(WaitText {
    category: StepCategory::Wait,
    pattern: r"^I wait for text (.+)$",
    description: "Wait for text to appear",
    example: "When I wait for text \"Ready\"",
    execute(page, caps, _table, _vars) {
        let text = q(&caps[1]);
        wait_for_text(page, &text, 30000).await?;
        Ok(None)
    }
});

step!(WaitMs {
    category: StepCategory::Wait,
    pattern: r"^I wait (\d+)\s*ms$",
    description: "Wait a fixed duration",
    example: "When I wait 500ms",
    execute(_page, caps, _table, _vars) {
        let ms: u64 = caps[1].parse().unwrap_or(0);
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        Ok(None)
    }
});

step!(WaitNavigation {
    category: StepCategory::Wait,
    pattern: r"^I wait for navigation$",
    description: "Wait for next navigation",
    example: "And I wait for navigation",
    execute(page, _caps, _table, _vars) {
        let _ = page.inner().wait_for_navigation().await;
        Ok(None)
    }
});

// ── Helpers ──

async fn wait_for_selector(page: &crate::page::Page, selector: &str, timeout_ms: u64) -> Result<(), String> {
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
  loop {
    if tokio::time::Instant::now() >= deadline {
      return Err(format!("Timeout ({timeout_ms}ms) waiting for '{selector}'"));
    }
    if super::find(page, selector).await.is_ok() {
      return Ok(());
    }
    tokio::time::sleep(std::time::Duration::from_millis(16)).await;
  }
}

async fn wait_for_text(page: &crate::page::Page, text: &str, timeout_ms: u64) -> Result<(), String> {
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
  let loc = page.locator("body");
  loop {
    if tokio::time::Instant::now() >= deadline {
      return Err(format!("Timeout ({timeout_ms}ms) waiting for text '{text}'"));
    }
    if let Ok(Some(content)) = loc.text_content().await {
      if content.contains(text) {
        return Ok(());
      }
    }
    tokio::time::sleep(std::time::Duration::from_millis(16)).await;
  }
}
