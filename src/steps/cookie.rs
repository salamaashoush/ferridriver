use super::{q, StepCategory, StepDef};
use chromiumoxide::cdp::browser_protocol::network::{CookieParam, DeleteCookiesParams};

pub fn register(steps: &mut Vec<Box<dyn StepDef>>) {
    steps.push(Box::new(SetCookieDomain));
    steps.push(Box::new(SetCookie));
    steps.push(Box::new(DeleteCookie));
    steps.push(Box::new(ClearCookies));
}

step!(SetCookieDomain {
    category: StepCategory::Cookie,
    pattern: r#"^I set cookie (.+) to (.+) on (.+)$"#,
    description: "Set cookie with domain",
    example: "When I set cookie \"token\" to \"abc\" on \"example.com\"",
    execute(page, caps, _table, _vars) {
        let name = q(&caps[1]);
        let value = q(&caps[2]);
        let domain = q(&caps[3]);
        let mut c = CookieParam::new(name, value);
        c.domain = Some(domain);
        page.set_cookie(c).await.map_err(|e| format!("{e}"))?;
        Ok(None)
    }
});

step!(SetCookie {
    category: StepCategory::Cookie,
    pattern: r#"^I set cookie (.+) to (.+)$"#,
    description: "Set cookie",
    example: "When I set cookie \"session\" to \"xyz\"",
    execute(page, caps, _table, _vars) {
        let name = q(&caps[1]);
        let value = q(&caps[2]);
        page.set_cookie(CookieParam::new(name, value)).await.map_err(|e| format!("{e}"))?;
        Ok(None)
    }
});

step!(DeleteCookie {
    category: StepCategory::Cookie,
    pattern: r#"^I delete cookie (.+)$"#,
    description: "Delete a cookie",
    example: "When I delete cookie \"session\"",
    execute(page, caps, _table, _vars) {
        let name = q(&caps[1]);
        page.delete_cookie(DeleteCookiesParams::new(name)).await.map_err(|e| format!("{e}"))?;
        Ok(None)
    }
});

step!(ClearCookies {
    category: StepCategory::Cookie,
    pattern: r#"^I clear all cookies$"#,
    description: "Clear all cookies",
    example: "When I clear all cookies",
    execute(page, _caps, _table, _vars) {
        page.clear_cookies().await.map_err(|e| format!("{e}"))?;
        Ok(None)
    }
});
