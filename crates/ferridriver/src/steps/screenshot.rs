use super::{StepCategory, StepDef, q};
use crate::backend::{ImageFormat, ScreenshotOpts};
use base64::Engine;

pub fn register(steps: &mut Vec<Box<dyn StepDef>>) {
  steps.push(Box::new(ScreenshotOf));
  steps.push(Box::new(Screenshot));
  steps.push(Box::new(Snapshot));
}

step!(Screenshot {
    category: StepCategory::Screenshot,
    pattern: r"^I take a screenshot$",
    description: "Take a full page screenshot",
    example: "Then I take a screenshot",
    execute(page, _caps, _table, _vars) {
        let bytes = page.screenshot(ScreenshotOpts::default())
            .await?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok(Some(serde_json::json!({"screenshot": b64, "format": "png"})))
    }
});

step!(ScreenshotOf {
    category: StepCategory::Screenshot,
    pattern: r"^I take a screenshot of (.+)$",
    description: "Screenshot a specific element",
    example: "Then I take a screenshot of \"#chart\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let el = super::find(page, &sel).await.map_err(|e| e.clone())?;
        let bytes = el.screenshot(ImageFormat::Png).await?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok(Some(serde_json::json!({"screenshot": b64, "selector": sel, "format": "png"})))
    }
});

step!(Snapshot {
    category: StepCategory::Screenshot,
    pattern: r"^I take a snapshot$",
    description: "Take accessibility tree snapshot",
    example: "Then I take a snapshot",
    execute(page, _caps, _table, _vars) {
        let nodes = page.accessibility_tree().await?;
        let (text, _) = crate::snapshot::build_snapshot(&nodes);
        Ok(Some(serde_json::Value::String(text)))
    }
});
