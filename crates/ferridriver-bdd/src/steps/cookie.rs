//! Cookie management step definitions using the BrowserContext cookie API
//! (matches Playwright's context.cookies / context.addCookies / context.clearCookies).

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver::backend::CookieData;
use ferridriver_bdd_macros::{step, when};

#[when("I set cookie {string} to {string}")]
async fn set_cookie(world: &mut BrowserWorld, name: String, value: String) {
  // Extract domain from current page URL so CDP accepts the cookie.
  let url = world.page().url().await.unwrap_or_default();
  let domain = url
    .split("://")
    .nth(1)
    .and_then(|s| s.split('/').next())
    .and_then(|s| s.split(':').next())
    .unwrap_or("")
    .to_string();

  world
    .context()
    .add_cookies(vec![CookieData {
      name,
      value,
      domain,
      path: "/".to_string(),
      secure: false,
      http_only: false,
      expires: None,
      same_site: None,
    }])
    .await
    .map_err(|e| StepError::from(format!("set cookie: {e}")))?;
}

#[when("I delete cookie {string}")]
async fn delete_cookie(world: &mut BrowserWorld, name: String) {
  world
    .context()
    .delete_cookie(&name, None)
    .await
    .map_err(|e| StepError::from(format!("delete cookie \"{name}\": {e}")))?;
}

#[step("I clear all cookies")]
async fn clear_cookies(world: &mut BrowserWorld) {
  world
    .context()
    .clear_cookies()
    .await
    .map_err(|e| StepError::from(format!("clear cookies: {e}")))?;
}
