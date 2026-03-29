use super::{q, js_escape, StepCategory, StepDef};
use crate::backend::AnyElement;

pub fn register(steps: &mut Vec<Box<dyn StepDef>>) {
    // Page-level assertions FIRST (more specific, avoids regex overlap with element-level)
    steps.push(Box::new(PageNotHasText));
    steps.push(Box::new(PageHasText));
    // URL / title
    steps.push(Box::new(UrlContains));
    steps.push(Box::new(UrlExact));
    steps.push(Box::new(TitleContains));
    steps.push(Box::new(TitleExact));
    // Visibility
    steps.push(Box::new(NotVisible));
    steps.push(Box::new(Visible));
    // Element text (negated first)
    steps.push(Box::new(NotContainsText));
    steps.push(Box::new(ContainsText));
    steps.push(Box::new(TextExact));
    // Value
    steps.push(Box::new(ValueExact));
    // Attributes / classes
    steps.push(Box::new(HasAttrValue));
    steps.push(Box::new(NotHasAttr));
    steps.push(Box::new(HasAttr));
    steps.push(Box::new(NotHasClass));
    steps.push(Box::new(HasClass));
    // State
    steps.push(Box::new(Disabled));
    steps.push(Box::new(Enabled));
    steps.push(Box::new(NotChecked));
    steps.push(Box::new(Checked));
    // Count
    steps.push(Box::new(ElementCount));
}

// ── Helpers ──
// All element-level helpers resolve via the selector engine (super::find)
// so rich selectors (role=, text=, etc.) work in assertions the same as interactions.

/// Get innerText from an element resolved via the selector engine.
async fn el_inner_text(page: &crate::backend::AnyPage, sel: &str) -> Result<String, String> {
    let el = super::find(page, sel).await.map_err(|_| format!("'{sel}' not found"))?;
    let r = el.call_js_fn_value("function() { return this.innerText || '' }").await?;
    Ok(r.and_then(|v| v.as_str().map(std::string::ToString::to_string)).unwrap_or_default())
}

/// Call a JS function on a resolved element and return bool.
async fn el_js_bool(el: &AnyElement, func: &str) -> Result<bool, String> {
    let r = el.call_js_fn_value(func).await?;
    Ok(r == Some(serde_json::Value::Bool(true)))
}

/// Check visibility using the selector engine. Returns false if element not found
/// or hidden by CSS (display:none, visibility:hidden, opacity:0, zero size).
async fn is_element_visible(page: &crate::backend::AnyPage, sel: &str) -> Result<bool, String> {
    let Ok(el) = super::find(page, sel).await else { return Ok(false) };
    el_js_bool(&el, "function() { \
        var s = getComputedStyle(this); \
        if (s.display === 'none' || s.visibility === 'hidden' || s.opacity === '0') return false; \
        var r = this.getBoundingClientRect(); \
        return r.width > 0 && r.height > 0; \
    }").await
}

// ── Page-level text ──

step!(PageHasText {
    category: StepCategory::Assertion,
    pattern: r"^the page should (?:have|contain) text (.+)$",
    description: "Assert page body contains text",
    example: "Then the page should contain text \"Welcome\"",
    execute(page, caps, _table, _vars) {
        let text = q(&caps[1]);
        let r = page.evaluate(&format!("document.body?.innerText?.includes('{}') || false", js_escape(&text))).await?;
        if r != Some(serde_json::Value::Bool(true)) {
            return Err(format!("Page does not contain text '{text}'"));
        }
        Ok(None)
    }
});

step!(PageNotHasText {
    category: StepCategory::Assertion,
    pattern: r"^the page should not (?:have|contain) text (.+)$",
    description: "Assert page body does not contain text",
    example: "Then the page should not contain text \"Error\"",
    execute(page, caps, _table, _vars) {
        let text = q(&caps[1]);
        let r = page.evaluate(&format!("document.body?.innerText?.includes('{}') || false", js_escape(&text))).await?;
        if r == Some(serde_json::Value::Bool(true)) {
            return Err(format!("Page contains text '{text}' but should not"));
        }
        Ok(None)
    }
});

// ── URL / title ──

step!(UrlContains {
    category: StepCategory::Assertion,
    pattern: r"^the URL should contain (.+)$",
    description: "Assert URL contains substring",
    example: "Then the URL should contain \"/dashboard\"",
    execute(page, caps, _table, _vars) {
        let expected = q(&caps[1]);
        let url = page.url().await.ok().flatten().unwrap_or_default();
        if !url.contains(&expected) {
            return Err(format!("URL '{url}' does not contain '{expected}'"));
        }
        Ok(None)
    }
});

step!(UrlExact {
    category: StepCategory::Assertion,
    pattern: r"^the URL should be (.+)$",
    description: "Assert exact URL",
    example: "Then the URL should be \"https://example.com/\"",
    execute(page, caps, _table, _vars) {
        let expected = q(&caps[1]);
        let url = page.url().await.ok().flatten().unwrap_or_default();
        if url != expected {
            return Err(format!("URL is '{url}', expected '{expected}'"));
        }
        Ok(None)
    }
});

step!(TitleContains {
    category: StepCategory::Assertion,
    pattern: r"^the title should contain (.+)$",
    description: "Assert title contains substring",
    example: "Then the title should contain \"Dashboard\"",
    execute(page, caps, _table, _vars) {
        let expected = q(&caps[1]);
        let title = page.title().await.ok().flatten().unwrap_or_default();
        if !title.contains(&expected) {
            return Err(format!("Title '{title}' does not contain '{expected}'"));
        }
        Ok(None)
    }
});

step!(TitleExact {
    category: StepCategory::Assertion,
    pattern: r"^the title should be (.+)$",
    description: "Assert exact title",
    example: "Then the title should be \"My App\"",
    execute(page, caps, _table, _vars) {
        let expected = q(&caps[1]);
        let title = page.title().await.ok().flatten().unwrap_or_default();
        if title != expected {
            return Err(format!("Title is '{title}', expected '{expected}'"));
        }
        Ok(None)
    }
});

// ── Visibility ──

step!(Visible {
    category: StepCategory::Assertion,
    pattern: r"^(.+) should be visible$",
    description: "Assert element exists and is visible (not hidden by CSS)",
    example: "Then \"#dialog\" should be visible",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let visible = is_element_visible(page, &sel).await?;
        if !visible {
            return Err(format!("'{sel}' is not visible"));
        }
        Ok(None)
    }
});

step!(NotVisible {
    category: StepCategory::Assertion,
    pattern: r"^(.+) should not be visible$",
    description: "Assert element does not exist or is hidden by CSS",
    example: "Then \"#spinner\" should not be visible",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let visible = is_element_visible(page, &sel).await?;
        if visible {
            return Err(format!("'{sel}' is visible but should not be"));
        }
        Ok(None)
    }
});

// ── Element text ──

step!(ContainsText {
    category: StepCategory::Assertion,
    pattern: r"^(.+) should contain text (.+)$",
    description: "Assert element contains text",
    example: "Then \"#message\" should contain text \"Success\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let expected = q(&caps[2]);
        let text = el_inner_text(page, &sel).await?;
        if !text.contains(&expected) {
            return Err(format!("'{sel}' text is '{text}', does not contain '{expected}'"));
        }
        Ok(None)
    }
});

step!(NotContainsText {
    category: StepCategory::Assertion,
    pattern: r"^(.+) should not contain text (.+)$",
    description: "Assert element does not contain text",
    example: "Then \"#status\" should not contain text \"Error\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let expected = q(&caps[2]);
        let text = el_inner_text(page, &sel).await?;
        if text.contains(&expected) {
            return Err(format!("'{sel}' text '{text}' contains '{expected}' but should not"));
        }
        Ok(None)
    }
});

step!(TextExact {
    category: StepCategory::Assertion,
    pattern: r"^(.+) should have text (.+)$",
    description: "Assert element has exact text",
    example: "Then \"h1\" should have text \"Welcome\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let expected = q(&caps[2]);
        let text = el_inner_text(page, &sel).await?;
        if text.trim() != expected.trim() {
            return Err(format!("'{sel}' text is '{text}', expected '{expected}'"));
        }
        Ok(None)
    }
});

step!(ValueExact {
    category: StepCategory::Assertion,
    pattern: r"^(.+) should have value (.+)$",
    description: "Assert input has value",
    example: "Then \"#email\" should have value \"test@example.com\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let expected = q(&caps[2]);
        let el = super::find(page, &sel).await.map_err(|_| format!("'{sel}' not found"))?;
        let r = el.call_js_fn_value("function() { return this.value || '' }").await?;
        let val = r.and_then(|v| v.as_str().map(std::string::ToString::to_string)).unwrap_or_default();
        if val != expected {
            return Err(format!("'{sel}' value is '{val}', expected '{expected}'"));
        }
        Ok(None)
    }
});

// ── Attributes / classes ──

step!(HasAttrValue {
    category: StepCategory::Assertion,
    pattern: r"^(.+) should have attribute (.+) with value (.+)$",
    description: "Assert attribute has value",
    example: "Then \"#link\" should have attribute \"href\" with value \"/about\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let attr = q(&caps[2]);
        let expected = q(&caps[3]);
        let el = super::find(page, &sel).await.map_err(|_| format!("'{sel}' not found"))?;
        let r = el.call_js_fn_value(&format!("function() {{ return this.getAttribute('{}') || '' }}", js_escape(&attr))).await?;
        let val = r.and_then(|v| v.as_str().map(std::string::ToString::to_string)).unwrap_or_default();
        if val != expected {
            return Err(format!("'{sel}' attribute '{attr}' is '{val}', expected '{expected}'"));
        }
        Ok(None)
    }
});

step!(HasAttr {
    category: StepCategory::Assertion,
    pattern: r"^(.+) should have attribute (.+)$",
    description: "Assert element has attribute",
    example: "Then \"#input\" should have attribute \"required\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let attr = q(&caps[2]);
        let el = super::find(page, &sel).await.map_err(|_| format!("'{sel}' not found"))?;
        if !el_js_bool(&el, &format!("function() {{ return this.hasAttribute('{}') }}", js_escape(&attr))).await? {
            return Err(format!("'{sel}' does not have attribute '{attr}'"));
        }
        Ok(None)
    }
});

step!(NotHasAttr {
    category: StepCategory::Assertion,
    pattern: r"^(.+) should not have attribute (.+)$",
    description: "Assert element lacks attribute",
    example: "Then \"#input\" should not have attribute \"disabled\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let attr = q(&caps[2]);
        let el = super::find(page, &sel).await.map_err(|_| format!("'{sel}' not found"))?;
        if el_js_bool(&el, &format!("function() {{ return this.hasAttribute('{}') }}", js_escape(&attr))).await? {
            return Err(format!("'{sel}' has attribute '{attr}' but should not"));
        }
        Ok(None)
    }
});

step!(HasClass {
    category: StepCategory::Assertion,
    pattern: r"^(.+) should have class (.+)$",
    description: "Assert element has CSS class",
    example: "Then \"#btn\" should have class \"active\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let cls = q(&caps[2]);
        let el = super::find(page, &sel).await.map_err(|_| format!("'{sel}' not found"))?;
        if !el_js_bool(&el, &format!("function() {{ return this.classList.contains('{}') }}", js_escape(&cls))).await? {
            return Err(format!("'{sel}' does not have class '{cls}'"));
        }
        Ok(None)
    }
});

step!(NotHasClass {
    category: StepCategory::Assertion,
    pattern: r"^(.+) should not have class (.+)$",
    description: "Assert element lacks CSS class",
    example: "Then \"#btn\" should not have class \"loading\"",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let cls = q(&caps[2]);
        let el = super::find(page, &sel).await.map_err(|_| format!("'{sel}' not found"))?;
        if el_js_bool(&el, &format!("function() {{ return this.classList.contains('{}') }}", js_escape(&cls))).await? {
            return Err(format!("'{sel}' has class '{cls}' but should not"));
        }
        Ok(None)
    }
});

// ── State ──

step!(Enabled {
    category: StepCategory::Assertion,
    pattern: r"^(.+) should be enabled$",
    description: "Assert element is enabled",
    example: "Then \"#submit\" should be enabled",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let el = super::find(page, &sel).await.map_err(|_| format!("'{sel}' not found"))?;
        if !el_js_bool(&el, "function() { return !this.disabled }").await? {
            return Err(format!("'{sel}' is disabled"));
        }
        Ok(None)
    }
});

step!(Disabled {
    category: StepCategory::Assertion,
    pattern: r"^(.+) should be disabled$",
    description: "Assert element is disabled",
    example: "Then \"#submit\" should be disabled",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let el = super::find(page, &sel).await.map_err(|_| format!("'{sel}' not found"))?;
        if !el_js_bool(&el, "function() { return this.disabled === true }").await? {
            return Err(format!("'{sel}' is not disabled"));
        }
        Ok(None)
    }
});

step!(Checked {
    category: StepCategory::Assertion,
    pattern: r"^(.+) should be checked$",
    description: "Assert checkbox is checked",
    example: "Then \"#agree\" should be checked",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let el = super::find(page, &sel).await.map_err(|_| format!("'{sel}' not found"))?;
        if !el_js_bool(&el, "function() { return this.checked === true }").await? {
            return Err(format!("'{sel}' is not checked"));
        }
        Ok(None)
    }
});

step!(NotChecked {
    category: StepCategory::Assertion,
    pattern: r"^(.+) should not be checked$",
    description: "Assert checkbox is not checked",
    example: "Then \"#agree\" should not be checked",
    execute(page, caps, _table, _vars) {
        let sel = q(&caps[1]);
        let el = super::find(page, &sel).await.map_err(|_| format!("'{sel}' not found"))?;
        if el_js_bool(&el, "function() { return this.checked === true }").await? {
            return Err(format!("'{sel}' is checked but should not be"));
        }
        Ok(None)
    }
});

step!(ElementCount {
    category: StepCategory::Assertion,
    pattern: r"^there should be (\d+) (.+)$",
    description: "Assert element count",
    example: "Then there should be 3 \".item\"",
    execute(page, caps, _table, _vars) {
        let expected: usize = caps[1].parse().map_err(|_| "Invalid count")?;
        let sel = q(&caps[2]);
        // ElementCount must use querySelectorAll for counting - rich selectors
        // don't have a count concept. CSS selectors are the right tool here.
        let r = page.evaluate(&format!("document.querySelectorAll('{}').length", js_escape(&sel)))
            .await?;
        #[allow(clippy::cast_possible_truncation)] // element count will never exceed usize
        let actual = r.and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        if actual != expected {
            return Err(format!("Found {actual} '{sel}', expected {expected}"));
        }
        Ok(None)
    }
});
