//! Snapshot / screenshot / aria matchers for `Expect<Locator>`. These
//! stay in the test runner because they need `TestInfo`-keyed snapshot
//! directories, the `image` crate, and the Playwright-bundled aria
//! renderer's YAML output format. Every other locator matcher lives in
//! [`ferridriver_expect`] (single source of truth).

use std::future::Future;
use std::time::Duration;

use ferridriver::Locator;
use ferridriver_expect::{Expect, ExpectContext, MatchError, poll_until as expect_poll_until};

use super::ScreenshotMatcherOptions;
use crate::model::TestFailure;

fn locator_ctx(locator: &Locator, method: &'static str, is_not: bool) -> ExpectContext {
  ExpectContext {
    method,
    subject: format!("locator('{}')", locator.selector()),
    is_not,
  }
}

async fn poll_until_test<F, Fut>(timeout: Duration, ctx: ExpectContext, check: F) -> Result<(), TestFailure>
where
  F: FnMut() -> Fut,
  Fut: Future<Output = Result<(), MatchError>>,
{
  expect_poll_until(timeout, ctx, check).await.map_err(Into::into)
}

/// Snapshot matchers for `expect(locator)`. Imported via
/// `use ferridriver_test::expect::LocatorSnapshotMatchers;` at the call
/// site so the methods light up alongside the shared web-first
/// matchers from [`ferridriver_expect`].
#[allow(async_fn_in_trait)]
pub trait LocatorSnapshotMatchers {
  /// Compare the element's text content against a stored `.snap` file.
  async fn to_match_snapshot(&self, name: &str) -> Result<(), TestFailure>;

  /// Compare the element's screenshot to a baseline PNG (default
  /// options).
  async fn to_have_screenshot(&self, name: &str) -> Result<(), TestFailure>;

  /// Playwright `toHaveScreenshot(name, options?)` — full capture
  /// option bag.
  async fn to_have_screenshot_with(&self, name: &str, options: ScreenshotMatcherOptions) -> Result<(), TestFailure>;

  /// Playwright `toMatchAriaSnapshot(yaml)` — compares the live ARIA
  /// tree against the Playwright-style YAML template.
  async fn to_match_aria_snapshot(&self, expected_yaml: &str) -> Result<(), TestFailure>;
}

impl LocatorSnapshotMatchers for Expect<'_, Locator> {
  async fn to_match_snapshot(&self, name: &str) -> Result<(), TestFailure> {
    let locator = self.subject;
    let actual = locator.text_content().await.unwrap_or(None).unwrap_or_default();
    let snap_dir = std::path::PathBuf::from("__snapshots__");
    let update = std::env::var("UPDATE_SNAPSHOTS").is_ok();
    let info = crate::model::TestInfo {
      test_id: crate::model::TestId {
        file: String::new(),
        suite: None,
        name: name.to_string(),
        line: None,
      },
      title_path: vec![name.to_string()],
      retry: 0,
      worker_index: 0,
      parallel_index: 0,
      repeat_each_index: 0,
      output_dir: std::path::PathBuf::from("test-results"),
      snapshot_dir: snap_dir,
      snapshot_path_template: None,
      update_snapshots: crate::config::UpdateSnapshotsMode::default(),
      ignore_snapshots: false,
      attachments: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
      steps: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
      soft_errors: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
      errors: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
      snapshot_suffix: std::sync::Arc::new(tokio::sync::Mutex::new(String::new())),
      column: None,
      project: None,
      config_snapshot: None,
      timeout: self.timeout,
      tags: Vec::new(),
      start_time: std::time::Instant::now(),
      event_bus: None,
      annotations: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
      trace_composite: std::sync::Arc::new(std::sync::Mutex::new(None)),
      trace_step_calls: std::sync::Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
    };
    crate::snapshot::assert_snapshot(&info, &actual, name, update)
  }

  async fn to_have_screenshot(&self, name: &str) -> Result<(), TestFailure> {
    self
      .to_have_screenshot_with(name, ScreenshotMatcherOptions::default())
      .await
  }

  async fn to_have_screenshot_with(&self, name: &str, options: ScreenshotMatcherOptions) -> Result<(), TestFailure> {
    let locator = self.subject;
    let actual_png = capture_with_options(locator, &options).await?;
    crate::snapshot::compare_screenshot_png_with(&actual_png, name, &options)
  }

  async fn to_match_aria_snapshot(&self, expected_yaml: &str) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    poll_until_test(
      self.timeout,
      locator_ctx(locator, "toMatchAriaSnapshot", is_not),
      || {
        let expected_yaml = expected_yaml.to_string();
        async move {
          let aria_tree = locator
            .evaluate(
              "el => { \
                 if (!el) return 'EMPTY'; \
                 const inj = window.__fd && window.__fd._injected; \
                 if (inj && typeof inj.ariaSnapshot === 'function') { \
                   try { return inj.ariaSnapshot(el, { mode: 'default' }); } catch (_) {} \
                 } \
                 function walk(node, indent) { \
                   let role = node.getAttribute('role') || node.tagName.toLowerCase(); \
                   let name = node.getAttribute('aria-label') || node.textContent?.trim()?.substring(0, 50) || ''; \
                   let line = indent + '- ' + role; \
                   if (name) line += ' \"' + name + '\"'; \
                   let lines = [line]; \
                   for (const child of node.children) { \
                     lines.push(...walk(child, indent + '  ')); \
                   } \
                   return lines; \
                 } \
                 return walk(el, '').join('\\n'); \
               }",
              ferridriver::protocol::SerializedArgument::default(),
              None,
            )
            .await
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| "EMPTY".into());

          let expected_nodes = parse_aria_yaml(&expected_yaml);
          let actual_nodes = parse_aria_yaml(&aria_tree);
          let lines_match = match_aria_template(&expected_nodes, &actual_nodes);

          if lines_match == is_not {
            Err(MatchError::new(
              format!("{}\n{expected_yaml}", if is_not { "not matching" } else { "matching" }),
              aria_tree,
            ))
          } else {
            Ok(())
          }
        }
      },
    )
    .await
  }
}

// ── Screenshot capture wrapper (§7.17 capture-time options) ─────────────────

async fn capture_with_options(locator: &Locator, options: &ScreenshotMatcherOptions) -> Result<Vec<u8>, TestFailure> {
  let page = locator.page();

  let mut style_blocks: Vec<String> = Vec::new();

  if options.animations.as_deref() == Some("disabled") {
    style_blocks.push(
      "*, *::before, *::after { \
        animation-duration: 0s !important; \
        animation-delay: 0s !important; \
        animation-iteration-count: 1 !important; \
        transition-duration: 0s !important; \
        transition-delay: 0s !important; \
      }"
      .to_string(),
    );
  }

  if options.caret.as_deref() == Some("hide") {
    style_blocks.push("html, body, * { caret-color: transparent !important; }".to_string());
  }

  if let Some(ref style_path) = options.style_path {
    match std::fs::read_to_string(style_path) {
      Ok(content) => style_blocks.push(content),
      Err(e) => {
        return Err(TestFailure {
          message: format!("toHaveScreenshot stylePath {} unreadable: {e}", style_path.display()),
          stack: None,
          diff: None,
          screenshot: None,
        });
      },
    }
  }

  let mask_color = options.mask_color.as_deref().unwrap_or("#FF00FF");
  if !options.mask.is_empty() {
    let mut mask_css = String::new();
    for selector in &options.mask {
      mask_css.push_str(selector);
      mask_css.push_str(" { background: ");
      mask_css.push_str(mask_color);
      mask_css.push_str(" !important; color: ");
      mask_css.push_str(mask_color);
      mask_css.push_str(" !important; }\n");
    }
    style_blocks.push(mask_css);
  }

  let token = "ferridriver-screenshot-capture";

  if !style_blocks.is_empty() {
    let combined = style_blocks.join("\n");
    let escaped = serde_json::to_string(&combined).unwrap_or_else(|_| "\"\"".to_string());
    let inject_script = format!(
      "(function() {{ \
        const s = document.createElement('style'); \
        s.setAttribute('data-{TOK}', '1'); \
        s.textContent = {ESC}; \
        document.head.appendChild(s); \
        return true; \
      }})()",
      TOK = token,
      ESC = escaped,
    );
    let _ = page
      .evaluate(
        &inject_script,
        ferridriver::protocol::SerializedArgument::default(),
        None,
      )
      .await
      .map_err(|e| TestFailure {
        message: format!("screenshot capture-options inject failed: {e}"),
        stack: None,
        diff: None,
        screenshot: None,
      })?;
  }

  let raw_png = locator.screenshot().await.map_err(|e| TestFailure {
    message: format!("screenshot failed: {e}"),
    stack: None,
    diff: None,
    screenshot: None,
  });

  if !style_blocks.is_empty() {
    let cleanup = format!(
      "(function() {{ \
        document.querySelectorAll('style[data-{TOK}]').forEach(function(n) {{ n.remove(); }}); \
        return true; \
      }})()",
      TOK = token,
    );
    let _ = page
      .evaluate(&cleanup, ferridriver::protocol::SerializedArgument::default(), None)
      .await;
  }

  let png = raw_png?;

  if let Some(clip) = options.clip {
    Ok(crop_png_to_clip(&png, &clip)?)
  } else {
    Ok(png)
  }
}

fn crop_png_to_clip(png: &[u8], clip: &super::ScreenshotClip) -> Result<Vec<u8>, TestFailure> {
  use image::GenericImageView;

  let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).map_err(|e| TestFailure {
    message: format!("toHaveScreenshot clip: failed to decode capture: {e}"),
    stack: None,
    diff: None,
    screenshot: None,
  })?;
  let (img_w, img_h) = img.dimensions();
  #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
  let x = (clip.x.max(0.0).min(f64::from(img_w))) as u32;
  #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
  let y = (clip.y.max(0.0).min(f64::from(img_h))) as u32;
  #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
  let w = (clip.width.max(0.0).min(f64::from(img_w.saturating_sub(x)))) as u32;
  #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
  let h = (clip.height.max(0.0).min(f64::from(img_h.saturating_sub(y)))) as u32;
  if w == 0 || h == 0 {
    return Err(TestFailure {
      message: format!(
        "toHaveScreenshot clip: empty rect after clamping (x={x} y={y} w={w} h={h}) against {img_w}x{img_h} capture"
      ),
      stack: None,
      diff: None,
      screenshot: None,
    });
  }
  let cropped = img.crop_imm(x, y, w, h);
  let mut out = Vec::new();
  cropped
    .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
    .map_err(|e| TestFailure {
      message: format!("toHaveScreenshot clip: re-encode failed: {e}"),
      stack: None,
      diff: None,
      screenshot: None,
    })?;
  Ok(out)
}

// ── ARIA snapshot tree matcher (§7.17 toMatchAriaSnapshot) ─────────────────

#[derive(Debug, Clone, Default)]
struct AriaNode {
  role: String,
  name: Option<AriaName>,
  attrs: Vec<String>,
  children: Vec<AriaNode>,
}

#[derive(Debug, Clone)]
enum AriaName {
  Literal(String),
  Regex(regex::Regex),
}

impl AriaName {
  fn matches(&self, s: &str) -> bool {
    match self {
      Self::Literal(expected) => s.contains(expected),
      Self::Regex(re) => re.is_match(s),
    }
  }
}

fn parse_aria_yaml(input: &str) -> Vec<AriaNode> {
  let mut roots: Vec<AriaNode> = Vec::new();
  let mut path: Vec<(usize, Vec<usize>)> = Vec::new();

  for raw in input.lines() {
    let trimmed = raw.trim_end();
    let indent = trimmed.chars().take_while(|c| *c == ' ').count();
    let line = trimmed.trim_start();
    if line.is_empty() || !line.starts_with('-') {
      continue;
    }
    let body = line.trim_start_matches('-').trim_start();
    if body.starts_with("text:") {
      continue;
    }
    let node = parse_aria_line_body(body);
    while path.last().is_some_and(|(prev_indent, _)| *prev_indent >= indent) {
      path.pop();
    }
    let path_indices = if let Some((_, parent_path)) = path.last() {
      parent_path.clone()
    } else {
      Vec::new()
    };
    let mut children_holder: &mut Vec<AriaNode> = &mut roots;
    for &i in &path_indices {
      children_holder = &mut children_holder[i].children;
    }
    let new_index = children_holder.len();
    children_holder.push(node);
    let mut new_path = path_indices.clone();
    new_path.push(new_index);
    path.push((indent, new_path));
  }
  roots
}

fn parse_aria_line_body(body: &str) -> AriaNode {
  let mut body = body.trim_end_matches(':').trim_end();
  while body.ends_with(':') {
    body = body[..body.len() - 1].trim_end();
  }
  let mut node = AriaNode::default();
  let mut role_end = 0;
  for (i, c) in body.char_indices() {
    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
      role_end = i + c.len_utf8();
    } else {
      break;
    }
  }
  node.role = body[..role_end].to_string();
  let rest = body[role_end..].trim_start();

  let mut rest_owned: String = rest.to_string();
  while let Some(open) = rest_owned.find('[') {
    let Some(close_rel) = rest_owned[open..].find(']') else {
      break;
    };
    let close = open + close_rel;
    let attr = rest_owned[open + 1..close].trim().to_string();
    if !attr.is_empty() {
      node.attrs.push(attr);
    }
    rest_owned = format!("{}{}", &rest_owned[..open], &rest_owned[close + 1..]);
  }

  let rest = rest_owned.trim();
  if let Some(stripped) = rest.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
    node.name = Some(AriaName::Literal(stripped.to_string()));
  } else if let Some(stripped) = rest.strip_prefix('/').and_then(|s| {
    let last_slash = s.rfind('/')?;
    Some((&s[..last_slash], &s[last_slash + 1..]))
  }) {
    let (pattern, _flags) = stripped;
    if let Ok(re) = regex::Regex::new(pattern) {
      node.name = Some(AriaName::Regex(re));
    }
  } else if !rest.is_empty() && rest != ":" {
    node.name = Some(AriaName::Literal(rest.to_string()));
  }
  node
}

fn match_aria_template(expected: &[AriaNode], actual: &[AriaNode]) -> bool {
  let flat_actual = flatten_dfs(actual);
  let mut cursor = 0usize;
  for exp in expected {
    let mut matched = false;
    while cursor < flat_actual.len() {
      if matches_aria_node(exp, flat_actual[cursor]) {
        cursor += 1;
        matched = true;
        break;
      }
      cursor += 1;
    }
    if !matched {
      return false;
    }
  }
  true
}

fn flatten_dfs(roots: &[AriaNode]) -> Vec<&AriaNode> {
  let mut out: Vec<&AriaNode> = Vec::new();
  fn walk<'b>(node: &'b AriaNode, out: &mut Vec<&'b AriaNode>) {
    out.push(node);
    for child in &node.children {
      walk(child, out);
    }
  }
  for r in roots {
    walk(r, &mut out);
  }
  out
}

fn matches_aria_node(expected: &AriaNode, actual: &AriaNode) -> bool {
  if !expected.role.is_empty() && expected.role != actual.role {
    return false;
  }
  if let Some(ref name) = expected.name {
    let actual_name = match &actual.name {
      Some(AriaName::Literal(s)) => s.clone(),
      Some(AriaName::Regex(_)) | None => String::new(),
    };
    if !name.matches(&actual_name) {
      return false;
    }
  }
  for attr in &expected.attrs {
    if !actual.attrs.iter().any(|a| a == attr) {
      return false;
    }
  }
  if !expected.children.is_empty() && !match_aria_template(&expected.children, &actual.children) {
    return false;
  }
  true
}

#[cfg(test)]
mod aria_tests {
  use super::*;

  #[test]
  fn parses_simple_role_name_pairs() {
    let nodes = parse_aria_yaml(
      "- main\n  - heading \"Title\"\n  - button \"Click\"\n  - list:\n    - listitem \"One\"\n    - listitem \"Two\"\n",
    );
    assert_eq!(nodes.len(), 1);
    let main = &nodes[0];
    assert_eq!(main.role, "main");
    assert_eq!(main.children.len(), 3);
    assert_eq!(main.children[0].role, "heading");
    assert!(matches!(main.children[0].name, Some(AriaName::Literal(ref s)) if s == "Title"));
    let list = &main.children[2];
    assert_eq!(list.role, "list");
    assert_eq!(list.children.len(), 2);
    assert_eq!(list.children[0].role, "listitem");
  }

  #[test]
  fn parses_state_brackets() {
    let nodes = parse_aria_yaml("- button [disabled] \"Save\"");
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].role, "button");
    assert_eq!(nodes[0].attrs, vec!["disabled".to_string()]);
    assert!(matches!(nodes[0].name, Some(AriaName::Literal(ref s)) if s == "Save"));
  }

  #[test]
  fn enforces_ancestor_relationships() {
    let actual = parse_aria_yaml("- main\n  - toolbar\n    - button \"Cut\"\n  - list\n    - listitem \"Item\"\n");
    let expected = parse_aria_yaml("- main\n  - list\n    - button \"Cut\"\n");
    assert!(!match_aria_template(&expected, &actual));
  }

  #[test]
  fn accepts_descendant_under_correct_parent() {
    let actual = parse_aria_yaml("- main\n  - list\n    - listitem \"One\"\n    - listitem \"Two\"\n");
    let expected = parse_aria_yaml("- main\n  - list\n    - listitem \"Two\"\n");
    assert!(match_aria_template(&expected, &actual));
  }

  #[test]
  fn requires_state_to_be_present_on_actual() {
    let actual = parse_aria_yaml("- button \"Save\"");
    let expected = parse_aria_yaml("- button [disabled] \"Save\"");
    assert!(!match_aria_template(&expected, &actual));
  }

  #[test]
  fn matches_template_against_subtree_of_actual() {
    let actual = parse_aria_yaml("- main\n  - button \"Save\" [disabled]\n");
    let expected = parse_aria_yaml("- button [disabled] \"Save\"");
    assert!(match_aria_template(&expected, &actual));
  }
}
