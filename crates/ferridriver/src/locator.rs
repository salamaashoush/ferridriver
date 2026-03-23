//! Lazy element locator -- mirrors Playwright's Locator interface.
//!
//! A Locator stores a selector string and a reference to its Page.
//! It does NOT query the DOM when created. Resolution happens lazily
//! when an action method (click, fill, etc.) is called.
//!
//! Locators can be chained to narrow scope:
//! ```ignore
//! page.locator("css=.form").get_by_role("textbox", Default::default()).fill("hello").await?;
//! ```

use crate::actions;
use crate::backend::AnyElement;
use crate::options::*;
use crate::selectors;

/// A lazy element locator. Does not query the DOM until an action is called.
#[derive(Clone)]
pub struct Locator {
  pub(crate) page: crate::page::Page,
  pub(crate) selector: String,
}

impl Locator {
  // ── Sub-locators (chain with >>) ──────────────────────────────────────────

  /// Narrow this locator's scope with an additional selector.
  pub fn locator(&self, selector: &str) -> Locator {
    self.chain(selector)
  }

  pub fn get_by_role(&self, role: &str, opts: RoleOptions) -> Locator {
    self.chain(&build_role_selector(role, &opts))
  }

  pub fn get_by_text(&self, text: &str, opts: TextOptions) -> Locator {
    self.chain(&build_text_selector("text", text, &opts))
  }

  pub fn get_by_label(&self, text: &str, opts: TextOptions) -> Locator {
    self.chain(&build_text_selector("label", text, &opts))
  }

  pub fn get_by_placeholder(&self, text: &str, opts: TextOptions) -> Locator {
    self.chain(&build_text_selector("placeholder", text, &opts))
  }

  pub fn get_by_alt_text(&self, text: &str, opts: TextOptions) -> Locator {
    self.chain(&build_text_selector("alt", text, &opts))
  }

  pub fn get_by_title(&self, text: &str, opts: TextOptions) -> Locator {
    self.chain(&build_text_selector("title", text, &opts))
  }

  pub fn get_by_test_id(&self, test_id: &str) -> Locator {
    self.chain(&format!("testid={test_id}"))
  }

  pub fn first(&self) -> Locator {
    self.chain("nth=0")
  }

  pub fn last(&self) -> Locator {
    self.chain("nth=-1")
  }

  pub fn nth(&self, index: i32) -> Locator {
    self.chain(&format!("nth={index}"))
  }

  pub fn filter(&self, opts: FilterOptions) -> Locator {
    let mut loc = self.clone();
    if let Some(text) = &opts.has_text {
      loc = loc.chain(&format!("has-text={text}"));
    }
    if let Some(text) = &opts.has_not_text {
      loc = loc.chain(&format!("has-not-text={text}"));
    }
    if let Some(sel) = &opts.has {
      loc = loc.chain(&format!("has={sel}"));
    }
    if let Some(sel) = &opts.has_not {
      loc = loc.chain(&format!("has-not={sel}"));
    }
    loc
  }

  // ── Actions ───────────────────────────────────────────────────────────────

  pub async fn click(&self) -> Result<(), String> {
    let el = self.resolve().await?;
    // Click guard: prevent clicking <select> or file inputs
    if let Err(guard_err) = actions::check_click_guard(&el, self.page.inner()).await {
      return Err(guard_err.to_string());
    }
    actions::wait_for_actionable(&el, self.page.inner()).await.ok();
    el.click().await
  }

  pub async fn dblclick(&self) -> Result<(), String> {
    let el = self.resolve().await?;
    actions::wait_for_actionable(&el, self.page.inner()).await.ok();
    el.click().await?;
    el.click().await
  }

  pub async fn fill(&self, value: &str) -> Result<(), String> {
    let el = self.resolve().await?;
    actions::fill(&el, value).await
  }

  pub async fn clear(&self) -> Result<(), String> {
    let el = self.resolve().await?;
    let _ = el.call_js_fn("function() { \
      if (window.__fd) window.__fd.clearAndDispatch(this); \
      else { this.value = ''; } \
    }").await;
    Ok(())
  }

  pub async fn type_text(&self, text: &str) -> Result<(), String> {
    let el = self.resolve().await?;
    actions::wait_for_actionable(&el, self.page.inner()).await.ok();
    el.type_str(text).await
  }

  pub async fn press(&self, key: &str) -> Result<(), String> {
    self.resolve().await?;
    self.page.inner().press_key(key).await
  }

  pub async fn hover(&self) -> Result<(), String> {
    let el = self.resolve().await?;
    actions::wait_for_actionable(&el, self.page.inner()).await.ok();
    el.hover().await
  }

  pub async fn focus(&self) -> Result<(), String> {
    let el = self.resolve().await?;
    let _ = el.call_js_fn("function() { this.focus(); }").await;
    Ok(())
  }

  pub async fn check(&self) -> Result<(), String> {
    let el = self.resolve().await?;
    actions::wait_for_actionable(&el, self.page.inner()).await.ok();
    let _ = el.call_js_fn("function() { if (!this.checked) this.click(); }").await;
    Ok(())
  }

  pub async fn uncheck(&self) -> Result<(), String> {
    let el = self.resolve().await?;
    actions::wait_for_actionable(&el, self.page.inner()).await.ok();
    let _ = el.call_js_fn("function() { if (this.checked) this.click(); }").await;
    Ok(())
  }

  pub async fn select_option(&self, value: &str) -> Result<Vec<String>, String> {
    let el = self.resolve().await?;
    let result = actions::select_option(&el, self.page.inner(), value).await?;
    Ok(vec![result.selected_value])
  }

  pub async fn set_input_files(&self, paths: &[String]) -> Result<(), String> {
    actions::upload_file(self.page.inner(), &self.selector, paths).await
  }

  pub async fn scroll_into_view(&self) -> Result<(), String> {
    let el = self.resolve().await?;
    el.scroll_into_view().await
  }

  pub async fn dispatch_event(&self, event_type: &str) -> Result<(), String> {
    let el = self.resolve().await?;
    let _ = el.call_js_fn(&format!(
      "function() {{ this.dispatchEvent(new Event('{event_type}', {{bubbles: true}})); }}"
    )).await;
    Ok(())
  }

  // ── Content & state ───────────────────────────────────────────────────────

  pub async fn text_content(&self) -> Result<Option<String>, String> {
    self.eval_prop("textContent").await
  }

  pub async fn inner_text(&self) -> Result<String, String> {
    self.eval_prop("innerText").await.map(|v| v.unwrap_or_default())
  }

  pub async fn inner_html(&self) -> Result<String, String> {
    self.eval_prop("innerHTML").await.map(|v| v.unwrap_or_default())
  }

  pub async fn get_attribute(&self, name: &str) -> Result<Option<String>, String> {
    let el = self.resolve().await?;
    let escaped = name.replace('\'', "\\'");
    let _ = el.call_js_fn(&format!(
      "function() {{ this.setAttribute('data-fd-attr-result', this.getAttribute('{escaped}') || ''); }}"
    )).await;
    let val = self.page.inner().evaluate(&format!(
      "(function() {{ var e = document.querySelector('[data-fd-attr-result]'); \
       if (!e) return null; var v = e.getAttribute('data-fd-attr-result'); \
       e.removeAttribute('data-fd-attr-result'); return v; }})()"
    )).await?.and_then(|v| v.as_str().map(|s| s.to_string()));
    Ok(val)
  }

  pub async fn input_value(&self) -> Result<String, String> {
    self.eval_prop("value").await.map(|v| v.unwrap_or_default())
  }

  pub async fn is_visible(&self) -> Result<bool, String> {
    self.eval_bool("function() { \
      var s = getComputedStyle(this); \
      return s.display !== 'none' && s.visibility !== 'hidden' && s.opacity !== '0'; \
    }").await
  }

  pub async fn is_hidden(&self) -> Result<bool, String> {
    self.is_visible().await.map(|v| !v)
  }

  pub async fn is_enabled(&self) -> Result<bool, String> {
    self.eval_bool("function() { return !this.disabled; }").await
  }

  pub async fn is_disabled(&self) -> Result<bool, String> {
    self.eval_bool("function() { return !!this.disabled; }").await
  }

  pub async fn is_checked(&self) -> Result<bool, String> {
    self.eval_bool("function() { return !!this.checked; }").await
  }

  pub async fn count(&self) -> Result<usize, String> {
    let matches = selectors::query_all(self.page.inner(), &self.selector).await?;
    selectors::cleanup_tags(self.page.inner()).await;
    Ok(matches.len())
  }

  pub async fn bounding_box(&self) -> Result<Option<BoundingBox>, String> {
    let el = self.resolve().await?;
    let _ = el.call_js_fn("function() { \
      var r = this.getBoundingClientRect(); \
      this.setAttribute('data-fd-bbox', JSON.stringify({x:r.x,y:r.y,width:r.width,height:r.height})); \
    }").await;
    let json = self.page.inner().evaluate("(function() { \
      var e = document.querySelector('[data-fd-bbox]'); \
      if (!e) return null; \
      var v = e.getAttribute('data-fd-bbox'); \
      e.removeAttribute('data-fd-bbox'); \
      return v; \
    })()").await?.and_then(|v| v.as_str().map(|s| s.to_string()));
    match json {
      Some(s) => {
        let parsed: serde_json::Value = serde_json::from_str(&s).map_err(|e| format!("{e}"))?;
        Ok(Some(BoundingBox {
          x: parsed["x"].as_f64().unwrap_or(0.0),
          y: parsed["y"].as_f64().unwrap_or(0.0),
          width: parsed["width"].as_f64().unwrap_or(0.0),
          height: parsed["height"].as_f64().unwrap_or(0.0),
        }))
      }
      None => Ok(None),
    }
  }

  // ── Waiting ───────────────────────────────────────────────────────────────

  pub async fn wait_for(&self, opts: WaitOptions) -> Result<(), String> {
    let timeout = opts.timeout.unwrap_or(30000);
    let state = opts.state.as_deref().unwrap_or("visible");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout);

    loop {
      if tokio::time::Instant::now() >= deadline {
        return Err(format!("Timeout waiting for '{}' to be {state}", self.selector));
      }
      match state {
        "attached" | "visible" => {
          if selectors::query_one(self.page.inner(), &self.selector, false).await.is_ok() {
            selectors::cleanup_tags(self.page.inner()).await;
            return Ok(());
          }
        }
        "hidden" | "detached" => {
          if selectors::query_one(self.page.inner(), &self.selector, false).await.is_err() {
            return Ok(());
          }
          selectors::cleanup_tags(self.page.inner()).await;
        }
        _ => return Err(format!("Unknown wait state: {state}")),
      }
      tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
  }

  // ── Screenshot ────────────────────────────────────────────────────────────

  pub async fn screenshot(&self) -> Result<Vec<u8>, String> {
    let el = self.resolve().await?;
    el.screenshot(crate::backend::ImageFormat::Png).await
  }

  // ── Editable check ───────────────────────────────────────────────────────

  pub async fn is_editable(&self) -> Result<bool, String> {
    self.eval_bool("function() { return !this.disabled && !this.readOnly; }").await
  }

  // ── Blur ────────────────────────────────────────────────────────────────

  pub async fn blur(&self) -> Result<(), String> {
    let el = self.resolve().await?;
    let _ = el.call_js_fn("function() { this.blur(); }").await;
    Ok(())
  }

  // ── Press sequentially ──────────────────────────────────────────────────

  /// Type text character by character with a delay between each.
  pub async fn press_sequentially(&self, text: &str, delay_ms: Option<u64>) -> Result<(), String> {
    let el = self.resolve().await?;
    actions::wait_for_actionable(&el, self.page.inner()).await.ok();
    let delay = delay_ms.unwrap_or(50);
    for ch in text.chars() {
      self.page.inner().press_key(&ch.to_string()).await?;
      if delay > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
      }
    }
    Ok(())
  }

  // ── Drag to another locator ─────────────────────────────────────────────

  pub async fn drag_to(&self, target: &Locator) -> Result<(), String> {
    let source_el = self.resolve().await?;
    let _ = source_el.call_js_fn("function() { \
      var r = this.getBoundingClientRect(); \
      this.setAttribute('data-fd-drag-src', JSON.stringify({x:r.x+r.width/2, y:r.y+r.height/2})); \
    }").await;
    let target_el = target.resolve().await?;
    let _ = target_el.call_js_fn("function() { \
      var r = this.getBoundingClientRect(); \
      this.setAttribute('data-fd-drag-tgt', JSON.stringify({x:r.x+r.width/2, y:r.y+r.height/2})); \
    }").await;

    let src_json = self.page.inner().evaluate("(function() { \
      var e = document.querySelector('[data-fd-drag-src]'); \
      if (!e) return null; var v = e.getAttribute('data-fd-drag-src'); \
      e.removeAttribute('data-fd-drag-src'); return v; \
    })()").await?.and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default();

    let tgt_json = self.page.inner().evaluate("(function() { \
      var e = document.querySelector('[data-fd-drag-tgt]'); \
      if (!e) return null; var v = e.getAttribute('data-fd-drag-tgt'); \
      e.removeAttribute('data-fd-drag-tgt'); return v; \
    })()").await?.and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default();

    let src: serde_json::Value = serde_json::from_str(&src_json).map_err(|e| format!("{e}"))?;
    let tgt: serde_json::Value = serde_json::from_str(&tgt_json).map_err(|e| format!("{e}"))?;

    let from = (src["x"].as_f64().unwrap_or(0.0), src["y"].as_f64().unwrap_or(0.0));
    let to = (tgt["x"].as_f64().unwrap_or(0.0), tgt["y"].as_f64().unwrap_or(0.0));
    self.page.inner().click_and_drag(from, to).await
  }

  // ── Combinators ─────────────────────────────────────────────────────────

  /// Union: matches elements from either this or the other locator.
  pub fn or(&self, other: &Locator) -> Locator {
    // Use CSS :is() for combining if both are CSS, otherwise not easily composable
    // For now, just return self (limitation noted)
    // A proper implementation would need selector engine support for OR
    self.clone()
  }

  /// Intersection: matches elements that match both locators.
  pub fn and(&self, other: &Locator) -> Locator {
    // Chain with >> which narrows scope
    self.chain(&other.selector)
  }

  // ── All matches ─────────────────────────────────────────────────────────

  /// Return all matching locators as individual Locator instances.
  pub async fn all(&self) -> Result<Vec<Locator>, String> {
    let count = self.count().await?;
    let mut locators = Vec::with_capacity(count);
    for i in 0..count {
      locators.push(self.nth(i as i32));
    }
    Ok(locators)
  }

  /// Get text content of all matching elements.
  pub async fn all_text_contents(&self) -> Result<Vec<String>, String> {
    let matches = selectors::query_all(self.page.inner(), &self.selector).await?;
    selectors::cleanup_tags(self.page.inner()).await;
    Ok(matches.into_iter().map(|m| m.text).collect())
  }

  /// Get inner text of all matching elements.
  pub async fn all_inner_texts(&self) -> Result<Vec<String>, String> {
    // Same as all_text_contents for our implementation
    self.all_text_contents().await
  }

  // ── Selector access ───────────────────────────────────────────────────────

  pub fn selector(&self) -> &str {
    &self.selector
  }

  // ── Internal ──────────────────────────────────────────────────────────────

  async fn resolve(&self) -> Result<AnyElement, String> {
    selectors::query_one(self.page.inner(), &self.selector, false).await
  }

  fn chain(&self, sub: &str) -> Locator {
    let selector = if self.selector.is_empty() {
      sub.to_string()
    } else {
      format!("{} >> {sub}", self.selector)
    };
    Locator { page: self.page.clone(), selector }
  }

  async fn eval_prop(&self, prop: &str) -> Result<Option<String>, String> {
    let el = self.resolve().await?;
    let _ = el.call_js_fn(&format!(
      "function() {{ this.setAttribute('data-fd-prop', String(this.{prop} || '')); }}"
    )).await;
    let val = self.page.inner().evaluate(&format!(
      "(function() {{ var e = document.querySelector('[data-fd-prop]'); \
       if (!e) return null; var v = e.getAttribute('data-fd-prop'); \
       e.removeAttribute('data-fd-prop'); return v; }})()"
    )).await?.and_then(|v| v.as_str().map(|s| s.to_string()));
    Ok(val)
  }

  async fn eval_bool(&self, func: &str) -> Result<bool, String> {
    let el = self.resolve().await?;
    let _ = el.call_js_fn(&format!(
      "function() {{ this.setAttribute('data-fd-bool', ({func}).call(this) ? '1' : '0'); }}"
    )).await;
    let val = self.page.inner().evaluate(
      "(function() { var e = document.querySelector('[data-fd-bool]'); \
       if (!e) return '0'; var v = e.getAttribute('data-fd-bool'); \
       e.removeAttribute('data-fd-bool'); return v; })()"
    ).await?.and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default();
    Ok(val == "1")
  }
}

impl std::fmt::Debug for Locator {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Locator").field("selector", &self.selector).finish()
  }
}

// ── Selector builders ───────────────────────────────────────────────────────

pub(crate) fn build_role_selector(role: &str, opts: &RoleOptions) -> String {
  let mut sel = format!("role={role}");
  if let Some(name) = &opts.name {
    sel.push_str(&format!("[name=\"{}\"]", name.replace('"', "\\\"")));
  }
  if opts.exact == Some(true) {
    // exact name matching handled at the engine level
  }
  if let Some(true) = opts.checked {
    sel.push_str("[checked=true]");
  }
  if let Some(false) = opts.checked {
    sel.push_str("[checked=false]");
  }
  if let Some(true) = opts.disabled {
    sel.push_str("[disabled=true]");
  }
  if let Some(false) = opts.disabled {
    sel.push_str("[disabled=false]");
  }
  if let Some(true) = opts.expanded {
    sel.push_str("[expanded=true]");
  }
  if let Some(false) = opts.expanded {
    sel.push_str("[expanded=false]");
  }
  if let Some(level) = opts.level {
    sel.push_str(&format!("[level={level}]"));
  }
  if let Some(true) = opts.pressed {
    sel.push_str("[pressed=true]");
  }
  if let Some(true) = opts.selected {
    sel.push_str("[selected=true]");
  }
  if let Some(true) = opts.include_hidden {
    sel.push_str("[include-hidden=true]");
  }
  sel
}

fn build_text_selector(engine: &str, text: &str, opts: &TextOptions) -> String {
  if opts.exact == Some(true) {
    format!("{engine}=\"{text}\"")
  } else {
    format!("{engine}={text}")
  }
}
