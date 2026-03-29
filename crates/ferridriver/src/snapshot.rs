//! Accessibility tree snapshot — compact, LLM-friendly format.
//!
//! Supports:
//! - Depth-limited tree fetching (native CDP depth param / `NSAccessibility` depth)
//! - Incremental tracking: store previous snapshot per track key, return only changes
//! - Compatible with dev-browser's `snapshotForAI()` API shape

use crate::backend::{AnyPage, AxNodeData};
use rustc_hash::FxHashMap as HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};

static REF_COUNTER: AtomicUsize = AtomicUsize::new(1);

pub fn reset_refs() {
  REF_COUNTER.store(1, Ordering::SeqCst);
}

fn next_ref() -> String {
  format!("e{}", REF_COUNTER.fetch_add(1, Ordering::SeqCst))
}

const NOISE_ROLES: &[&str] = &[
  "none",
  "generic",
  "InlineTextBox",
  "LineBreak",
  "LayoutTable",
  "LayoutTableRow",
  "LayoutTableCell",
  "LayoutTableColumn",
  "LayoutTableBody",
];

const INTERACTIVE_ROLES: &[&str] = &[
  "link",
  "button",
  "textbox",
  "checkbox",
  "radio",
  "combobox",
  "menuitem",
  "tab",
  "switch",
  "slider",
  "spinbutton",
  "searchbox",
  "option",
  "menuitemcheckbox",
  "menuitemradio",
];

const SEMANTIC_ROLES: &[&str] = &[
  "heading",
  "paragraph",
  "list",
  "listitem",
  "navigation",
  "main",
  "banner",
  "contentinfo",
  "complementary",
  "form",
  "search",
  "article",
  "region",
  "dialog",
  "alertdialog",
  "alert",
  "table",
  "row",
  "cell",
  "columnheader",
  "rowheader",
  "img",
  "figure",
  "separator",
  "menu",
  "menubar",
  "toolbar",
  "tablist",
  "tabpanel",
  "tree",
  "treeitem",
  "grid",
  "status",
];

fn is_noise(role: &str) -> bool {
  NOISE_ROLES.contains(&role)
}

fn is_interactive(role: &str) -> bool {
  INTERACTIVE_ROLES.contains(&role)
}

fn needs_ref(role: &str) -> bool {
  is_interactive(role) || SEMANTIC_ROLES.contains(&role)
}

const MAX_SNAPSHOT_CHARS: usize = 15_000;
const MAX_TEXT_LEN: usize = 80;

// ─── SnapshotForAI types ─────────────────────────────────────────────────────

/// Options for `snapshot_for_ai()`.
#[derive(Debug, Clone, Default)]
pub struct SnapshotOptions {
  /// CDP/native depth limit for the accessibility tree fetch.
  /// None or -1 = unlimited. 0 = root only.
  pub depth: Option<i32>,
  /// Track key for incremental tracking. When set, subsequent calls with the
  /// same key return only changed/new nodes in the `incremental` field.
  pub track: Option<String>,
}

/// Result of `snapshot_for_ai()`.
#[derive(Debug, Clone)]
pub struct SnapshotForAI {
  /// Full accessibility tree snapshot text (always present).
  pub full: String,
  /// Incremental snapshot containing only changed/new nodes since the last
  /// call with the same track key. None on first call or when nothing changed.
  pub incremental: Option<String>,
  /// Map of ref labels (e.g. "e5") to backend DOM node IDs for element resolution.
  pub ref_map: HashMap<String, i64>,
}

/// Per-node fingerprint for incremental tracking.
/// Captures the identity of a rendered node (role + name + properties).
fn node_fingerprint(node: &AxNodeData) -> u64 {
  let mut hasher = DefaultHasher::new();
  node.role.hash(&mut hasher);
  node.name.hash(&mut hasher);
  node.description.hash(&mut hasher);
  for prop in &node.properties {
    prop.name.hash(&mut hasher);
    if let Some(val) = &prop.value {
      val.to_string().hash(&mut hasher);
    }
  }
  hasher.finish()
}

/// Incremental tracker state — stores fingerprints from the previous snapshot.
#[derive(Debug, Clone, Default)]
pub struct SnapshotTracker {
  /// `track_key` -> Vec of (`node_id`, fingerprint) from previous call.
  tracks: HashMap<String, Vec<(String, u64)>>,
}

impl SnapshotTracker {
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  /// Compute incremental diff. Returns the set of node IDs that are new or changed.
  /// Updates stored fingerprints for the track key.
  fn compute_diff(&mut self, track_key: &str, nodes: &[AxNodeData]) -> Option<std::collections::HashSet<String>> {
    let current: Vec<(String, u64)> = nodes
      .iter()
      .filter(|n| !n.ignored)
      .map(|n| (n.node_id.clone(), node_fingerprint(n)))
      .collect();

    let prev = self.tracks.get(track_key);

    let changed = if let Some(prev_fingerprints) = prev {
      // Build a map of previous node_id -> fingerprint
      let prev_map: HashMap<&str, u64> = prev_fingerprints.iter().map(|(id, fp)| (id.as_str(), *fp)).collect();

      let changed_ids: std::collections::HashSet<String> = current
        .iter()
        .filter(|(id, fp)| {
          match prev_map.get(id.as_str()) {
            Some(prev_fp) => prev_fp != fp, // changed
            None => true,                   // new
          }
        })
        .map(|(id, _)| id.clone())
        .collect();

      if changed_ids.is_empty() {
        None
      } else {
        Some(changed_ids)
      }
    } else {
      // First call with this track key — no incremental
      None
    };

    // Store current fingerprints
    self.tracks.insert(track_key.to_string(), current);

    changed
  }
}

// ─── Snapshot building ───────────────────────────────────────────────────────

/// Build a compact snapshot. Returns (text, `ref_map`).
#[must_use]
pub fn build_snapshot(nodes: &[AxNodeData]) -> (String, HashMap<String, i64>) {
  build_snapshot_filtered(nodes, None)
}

/// Mutable context passed through snapshot tree traversal to reduce argument count.
struct SnapshotCtx<'a> {
  nodes: &'a [AxNodeData],
  children_map: HashMap<&'a str, Vec<usize>>,
  output: String,
  ref_map: HashMap<String, i64>,
  truncated: bool,
  filter_ids: Option<&'a std::collections::HashSet<String>>,
  relevant_nodes: Option<std::collections::HashSet<&'a str>>,
}

fn get_role(node: &AxNodeData) -> &str {
  node.role.as_deref().unwrap_or("generic")
}

fn get_name(node: &AxNodeData) -> &str {
  node.name.as_deref().unwrap_or("")
}

fn get_desc(node: &AxNodeData) -> &str {
  node.description.as_deref().unwrap_or("")
}

/// Build a compact snapshot, optionally filtering to only include specific node IDs.
/// When `filter_ids` is Some, only nodes whose ID is in the set (and their ancestor
/// chain context) are rendered.
fn build_snapshot_filtered(
  nodes: &[AxNodeData],
  filter_ids: Option<&std::collections::HashSet<String>>,
) -> (String, HashMap<String, i64>) {
  let mut children_map: HashMap<&str, Vec<usize>> = HashMap::default();
  for (i, node) in nodes.iter().enumerate() {
    if let Some(pid) = &node.parent_id {
      children_map.entry(pid.as_str()).or_default().push(i);
    }
  }

  // When filtering, pre-compute which nodes are in the subtree leading to changed nodes.
  // We include a changed node and all its ancestors so the tree context is preserved.
  let relevant_nodes: Option<std::collections::HashSet<&str>> = filter_ids.map(|changed| {
    let mut relevant = std::collections::HashSet::new();
    // Build parent lookup
    let parent_map: HashMap<&str, &str> = nodes
      .iter()
      .filter_map(|n| n.parent_id.as_ref().map(|pid| (n.node_id.as_str(), pid.as_str())))
      .collect();

    for id in changed {
      let mut cur = id.as_str();
      loop {
        if !relevant.insert(cur) {
          break;
        } // already visited
        match parent_map.get(cur) {
          Some(pid) => cur = pid,
          None => break,
        }
      }
    }
    relevant
  });

  let roots: Vec<usize> = nodes
    .iter()
    .enumerate()
    .filter(|(_, n)| n.parent_id.is_none() && !n.ignored)
    .map(|(i, _)| i)
    .collect();

  let mut ctx = SnapshotCtx {
    nodes,
    children_map,
    output: String::with_capacity(nodes.len() * 64),
    ref_map: HashMap::default(),
    truncated: false,
    filter_ids,
    relevant_nodes,
  };

  for root_idx in roots {
    format_tree(&mut ctx, root_idx, 0);
  }

  (ctx.output, ctx.ref_map)
}

fn format_tree(ctx: &mut SnapshotCtx<'_>, idx: usize, depth: usize) {
  use std::fmt::Write;

  if ctx.truncated {
    return;
  }
  if ctx.output.len() > MAX_SNAPSHOT_CHARS {
    ctx.truncated = true;
    ctx
      .output
      .push_str("\n... (snapshot truncated, page has more content)\n");
    return;
  }

  let node = &ctx.nodes[idx];

  // If filtering, skip nodes not in the relevant set
  if let Some(relevant) = &ctx.relevant_nodes {
    if !relevant.contains(node.node_id.as_str()) {
      return;
    }
  }

  if node.ignored {
    recurse_children(ctx, idx, depth);
    return;
  }

  let role = get_role(node);
  let name = get_name(node);
  let desc = get_desc(node);

  if is_noise(role) {
    recurse_children(ctx, idx, depth);
    return;
  }

  if role == "StaticText" {
    return;
  }

  if role == "RootWebArea" {
    recurse_children(ctx, idx, depth);
    return;
  }

  if !is_interactive(role) && !SEMANTIC_ROLES.contains(&role) && name.is_empty() {
    recurse_children(ctx, idx, depth);
    return;
  }

  // For incremental: only render leaf detail if this node is actually changed
  let is_changed_node = ctx.filter_ids.is_none_or(|ids| ids.contains(&node.node_id));

  let indent = "  ".repeat(depth);

  let ref_str = if needs_ref(role) || is_interactive(role) {
    let r = next_ref();
    if let Some(bid) = node.backend_dom_node_id {
      ctx.ref_map.insert(r.clone(), bid);
    }
    format!(" [ref={r}]")
  } else {
    String::new()
  };

  if is_changed_node {
    let _ = write!(ctx.output, "{indent}- {role}");
    write_node_name(&mut ctx.output, name);
    ctx.output.push_str(&ref_str);
    write_node_properties(&mut ctx.output, node, role);
    write_node_value(&mut ctx.output, node, role);
    write_node_description(&mut ctx.output, desc, name);
    ctx.output.push('\n');
  } else {
    // Ancestor context line -- abbreviated, just role + name for structure
    let _ = write!(ctx.output, "{indent}- {role}");
    if !name.is_empty() {
      let truncated_name = if name.len() > 30 { &name[..30] } else { name };
      let _ = write!(ctx.output, " \"{truncated_name}\"");
    }
    ctx.output.push_str(&ref_str);
    ctx.output.push_str(" ...\n");
  }

  recurse_children(ctx, idx, depth + 1);
}

/// Write the node name, truncating if necessary.
fn write_node_name(output: &mut String, name: &str) {
  use std::fmt::Write;
  if !name.is_empty() {
    if name.len() > MAX_TEXT_LEN {
      let _ = write!(output, " \"{}...\"", &name[..MAX_TEXT_LEN]);
    } else {
      let _ = write!(output, " \"{name}\"");
    }
  }
}

/// Write ARIA/semantic properties (level, url, checked, etc.) for a node.
fn write_node_properties(output: &mut String, node: &AxNodeData, role: &str) {
  use std::fmt::Write;
  for prop in &node.properties {
    if let Some(val) = &prop.value {
      match prop.name.as_str() {
        "level" => {
          let _ = write!(output, " [level={val}]");
        },
        "url" if is_interactive(role) => {
          let u = val.as_str().unwrap_or("");
          if !u.is_empty() && u.len() <= 100 {
            let _ = write!(output, " [url={u}]");
          }
        },
        "checked" if val.as_bool() == Some(true) => output.push_str(" [checked]"),
        "selected" if val.as_bool() == Some(true) => output.push_str(" [selected]"),
        "expanded" => {
          let _ = write!(output, " [expanded={val}]");
        },
        "disabled" if val.as_bool() == Some(true) => output.push_str(" [disabled]"),
        "required" if val.as_bool() == Some(true) => output.push_str(" [required]"),
        "focused" if val.as_bool() == Some(true) => output.push_str(" [focused]"),
        "readonly" if val.as_bool() == Some(true) => output.push_str(" [readonly]"),
        _ => {},
      }
    }
  }
}

/// Write the current value for input-like roles (textbox, combobox, etc.).
fn write_node_value(output: &mut String, node: &AxNodeData, role: &str) {
  use std::fmt::Write;
  if matches!(role, "textbox" | "combobox" | "searchbox" | "spinbutton") {
    for prop in &node.properties {
      if prop.name == "value" {
        if let Some(val) = &prop.value {
          if let Some(s) = val.as_str() {
            if !s.is_empty() {
              let display_val = if s.len() > 50 { &s[..50] } else { s };
              let _ = write!(output, " [value=\"{display_val}\"]");
            }
          }
        }
        break;
      }
    }
  }
}

/// Write the node description if it differs from the name.
fn write_node_description(output: &mut String, desc: &str, name: &str) {
  use std::fmt::Write;
  if !desc.is_empty() && desc != name {
    let d = if desc.len() > MAX_TEXT_LEN {
      &desc[..MAX_TEXT_LEN]
    } else {
      desc
    };
    let _ = write!(output, ": {d}");
  }
}

fn recurse_children(ctx: &mut SnapshotCtx<'_>, idx: usize, depth: usize) {
  if let Some(kids) = ctx.children_map.get(ctx.nodes[idx].node_id.as_str()) {
    let kids = kids.clone();
    for kid_idx in kids {
      format_tree(ctx, kid_idx, depth);
    }
  }
}

/// Build a `SnapshotForAI` with page context header, optional depth limiting,
/// and incremental change tracking. This is the single unified snapshot API.
///
/// The `full` field includes a page header (URL, title, scroll position,
/// console error count) followed by the accessibility tree snapshot.
///
/// # Errors
///
/// Returns an error if the accessibility tree cannot be fetched from the page.
pub async fn build_snapshot_for_ai(
  page: &AnyPage,
  opts: &SnapshotOptions,
  tracker: &mut SnapshotTracker,
) -> Result<SnapshotForAI, String> {
  use std::fmt::Write;

  let depth = opts.depth.unwrap_or(-1);

  // Page context header
  let url = page.url().await.ok().flatten().unwrap_or_default();
  let title = page.title().await.ok().flatten().unwrap_or_default();
  let console_errors = crate::actions::console_error_count(page).await;

  let mut header = String::new();
  header.push_str("### Page\n");
  let _ = writeln!(header, "- URL: {url}");
  let _ = writeln!(header, "- Title: {title}");
  if console_errors > 0 {
    let _ = writeln!(header, "- Console: {console_errors} errors");
  }
  if let Ok(si) = crate::actions::scroll_info(page).await {
    if si.scroll_height > 0 {
      let _ = writeln!(
        header,
        "- Scroll: {}/{}px (viewport: {}px)",
        si.scroll_y, si.scroll_height, si.viewport_height
      );
    }
  }
  header.push_str("\n### Snapshot\n");

  // Fetch accessibility tree with native depth limiting
  reset_refs();
  let tree = page.accessibility_tree_with_depth(depth).await?;

  // Build full snapshot
  let (snapshot_text, ref_map) = build_snapshot(&tree);
  let full = format!("{header}{snapshot_text}");

  // Incremental tracking
  let incremental = if let Some(track_key) = &opts.track {
    if let Some(changed_ids) = tracker.compute_diff(track_key, &tree) {
      // Re-render with only changed nodes (plus ancestor context)
      reset_refs();
      let (inc_text, _) = build_snapshot_filtered(&tree, Some(&changed_ids));
      if inc_text.is_empty() { None } else { Some(inc_text) }
    } else {
      None
    }
  } else {
    None
  };

  Ok(SnapshotForAI {
    full,
    incremental,
    ref_map,
  })
}
