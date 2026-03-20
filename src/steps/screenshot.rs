use super::{q, StepCategory, StepDef};
use base64::Engine;
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;

pub fn register(steps: &mut Vec<Box<dyn StepDef>>) {
    steps.push(Box::new(ScreenshotOf));
    steps.push(Box::new(Screenshot));
    steps.push(Box::new(Snapshot));
}

step!(Screenshot {
    category: StepCategory::Screenshot,
    pattern: r#"^I take a screenshot$"#,
    description: "Take a full page screenshot",
    example: "Then I take a screenshot",
    execute(page, _caps, _table, _vars) {
        let bytes = page.screenshot(ScreenshotParams::builder().build())
            .await.map_err(|e| format!("{e}"))?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok(Some(serde_json::json!({"screenshot": b64, "format": "png"})))
    }
});

step!(ScreenshotOf {
    category: StepCategory::Screenshot,
    pattern: r#"^I take a screenshot of (.+)$"#,
    description: "Screenshot a specific element",
    example: "Then I take a screenshot of \"#chart\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let el = page.find_element(&sel).await.map_err(|e| format!("{e}"))?;
        let bytes = el.screenshot(CaptureScreenshotFormat::Png).await.map_err(|e| format!("{e}"))?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok(Some(serde_json::json!({"screenshot": b64, "selector": sel, "format": "png"})))
    }
});

step!(Snapshot {
    category: StepCategory::Screenshot,
    pattern: r#"^I take a snapshot$"#,
    description: "Take accessibility tree snapshot",
    example: "Then I take a snapshot",
    execute(page, _caps, _table, _vars) {
        let tree = page.get_full_ax_tree(Some(-1), None).await.map_err(|e| format!("{e}"))?;
        let (text, _) = crate::snapshot::build_snapshot(&tree.nodes);
        Ok(Some(serde_json::Value::String(text)))
    }
});
