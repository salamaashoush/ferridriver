//! Accessibility tree snapshot — compact, LLM-friendly format.
//!
//! Goals:
//! - Minimize tokens: skip noise nodes, collapse redundant nesting
//! - Maximize signal: show only interactive/semantic elements with refs
//! - Cap output size: truncate large pages with a note
//!
//! Output example:
//! ```text
//! - heading "Example Domain" [ref=e1] [level=1]
//! - paragraph: This domain is for use in illustrative examples.
//! - link "More info" [ref=e2] [url=https://iana.org/...]
//! ```

use chromiumoxide::cdp::browser_protocol::accessibility::AxNode;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

static REF_COUNTER: AtomicUsize = AtomicUsize::new(1);

pub fn reset_refs() {
    REF_COUNTER.store(1, Ordering::SeqCst);
}

fn next_ref() -> String {
    format!("e{}", REF_COUNTER.fetch_add(1, Ordering::SeqCst))
}

/// Roles that are pure layout noise — skip and flatten children up.
const NOISE_ROLES: &[&str] = &[
    "none", "generic", "InlineTextBox", "LineBreak",
    "LayoutTable", "LayoutTableRow", "LayoutTableCell",
    "LayoutTableColumn", "LayoutTableBody",
];

/// Roles that are meaningful — always keep and assign refs.
const INTERACTIVE_ROLES: &[&str] = &[
    "link", "button", "textbox", "checkbox", "radio", "combobox",
    "menuitem", "tab", "switch", "slider", "spinbutton",
    "searchbox", "option", "menuitemcheckbox", "menuitemradio",
];

/// Roles that are structural — keep but don't need refs unless interactive.
const SEMANTIC_ROLES: &[&str] = &[
    "heading", "paragraph", "list", "listitem", "navigation", "main",
    "banner", "contentinfo", "complementary", "form", "search",
    "article", "region", "dialog", "alertdialog", "alert",
    "table", "row", "cell", "columnheader", "rowheader",
    "img", "figure", "separator", "menu", "menubar", "toolbar",
    "tablist", "tabpanel", "tree", "treeitem", "grid", "status",
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

/// Max output chars before truncation.
const MAX_SNAPSHOT_CHARS: usize = 15_000;

/// Max text content shown inline per node.
const MAX_TEXT_LEN: usize = 80;

/// Build a compact snapshot. Returns (text, ref_map).
pub fn build_snapshot(nodes: &[AxNode]) -> (String, HashMap<String, i64>) {
    let mut output = String::new();
    let mut ref_map: HashMap<String, i64> = HashMap::new();
    let mut truncated = false;

    // Index: parent → children
    let mut children_map: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, node) in nodes.iter().enumerate() {
        if let Some(pid) = &node.parent_id {
            children_map.entry(pid.inner().clone()).or_default().push(i);
        }
    }

    // Roots
    let roots: Vec<usize> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.parent_id.is_none() && !n.ignored)
        .map(|(i, _)| i)
        .collect();

    fn get_role(node: &AxNode) -> &str {
        node.role.as_ref().and_then(|v| v.value.as_ref()).and_then(|v| v.as_str()).unwrap_or("generic")
    }

    fn get_name(node: &AxNode) -> &str {
        node.name.as_ref().and_then(|v| v.value.as_ref()).and_then(|v| v.as_str()).unwrap_or("")
    }

    fn get_desc(node: &AxNode) -> &str {
        node.description.as_ref().and_then(|v| v.value.as_ref()).and_then(|v| v.as_str()).unwrap_or("")
    }

    fn format_tree(
        nodes: &[AxNode],
        children_map: &HashMap<String, Vec<usize>>,
        idx: usize,
        depth: usize,
        output: &mut String,
        ref_map: &mut HashMap<String, i64>,
        truncated: &mut bool,
    ) {
        if *truncated {
            return;
        }
        if output.len() > MAX_SNAPSHOT_CHARS {
            *truncated = true;
            output.push_str("\n... (snapshot truncated, page has more content)\n");
            return;
        }

        let node = &nodes[idx];
        if node.ignored {
            recurse_children(nodes, children_map, idx, depth, output, ref_map, truncated);
            return;
        }

        let role = get_role(node);
        let name = get_name(node);
        let desc = get_desc(node);

        // Skip noise roles — flatten children up
        if is_noise(role) {
            recurse_children(nodes, children_map, idx, depth, output, ref_map, truncated);
            return;
        }

        // Skip StaticText — its text is already in the parent's name
        if role == "StaticText" {
            return;
        }

        // Skip RootWebArea's direct output (just recurse)
        if role == "RootWebArea" {
            recurse_children(nodes, children_map, idx, depth, output, ref_map, truncated);
            return;
        }

        // For non-interactive semantic nodes with no useful name, flatten
        if !is_interactive(role) && !SEMANTIC_ROLES.contains(&role) && name.is_empty() {
            recurse_children(nodes, children_map, idx, depth, output, ref_map, truncated);
            return;
        }

        let indent = "  ".repeat(depth);

        // Assign ref
        let ref_str = if needs_ref(role) || is_interactive(role) {
            let r = next_ref();
            if let Some(bid) = node.backend_dom_node_id {
                ref_map.insert(r.clone(), *bid.inner());
            }
            format!(" [ref={r}]")
        } else {
            String::new()
        };

        // Build line
        output.push_str(&format!("{indent}- {role}"));

        // Name (truncate if long)
        if !name.is_empty() {
            let display_name = if name.len() > MAX_TEXT_LEN {
                format!("{}...", &name[..MAX_TEXT_LEN])
            } else {
                name.to_string()
            };
            output.push_str(&format!(" \"{display_name}\""));
        }

        output.push_str(&ref_str);

        // Key properties
        if let Some(props) = &node.properties {
            for prop in props {
                let pname = format!("{:?}", prop.name).to_lowercase();
                if let Some(val) = &prop.value.value {
                    match pname.as_str() {
                        "level" => output.push_str(&format!(" [level={val}]")),
                        "url" if is_interactive(role) => {
                            let u = val.as_str().unwrap_or("");
                            if !u.is_empty() && u.len() <= 100 {
                                output.push_str(&format!(" [url={u}]"));
                            }
                        }
                        "checked" if val.as_bool() == Some(true) => output.push_str(" [checked]"),
                        "selected" if val.as_bool() == Some(true) => output.push_str(" [selected]"),
                        "expanded" => output.push_str(&format!(" [expanded={val}]")),
                        "disabled" if val.as_bool() == Some(true) => output.push_str(" [disabled]"),
                        "required" if val.as_bool() == Some(true) => output.push_str(" [required]"),
                        "focused" if val.as_bool() == Some(true) => output.push_str(" [focused]"),
                        "readonly" if val.as_bool() == Some(true) => output.push_str(" [readonly]"),
                        _ => {}
                    }
                }
            }
        }

        // Description
        if !desc.is_empty() && desc != name {
            let d = if desc.len() > MAX_TEXT_LEN { &desc[..MAX_TEXT_LEN] } else { desc };
            output.push_str(&format!(": {d}"));
        }

        output.push('\n');

        // Recurse children
        recurse_children(nodes, children_map, idx, depth + 1, output, ref_map, truncated);
    }

    fn recurse_children(
        nodes: &[AxNode],
        children_map: &HashMap<String, Vec<usize>>,
        idx: usize,
        depth: usize,
        output: &mut String,
        ref_map: &mut HashMap<String, i64>,
        truncated: &mut bool,
    ) {
        if let Some(kids) = children_map.get(nodes[idx].node_id.inner()) {
            for &kid_idx in kids {
                format_tree(nodes, children_map, kid_idx, depth, output, ref_map, truncated);
            }
        }
    }

    for root_idx in roots {
        format_tree(nodes, &children_map, root_idx, 0, &mut output, &mut ref_map, &mut truncated);
    }

    (output, ref_map)
}

/// Build page context header + snapshot.
pub async fn page_context_with_snapshot(
    page: &chromiumoxide::Page,
) -> Result<(String, HashMap<String, i64>), String> {
    let url = page.url().await.ok().flatten().unwrap_or_default();
    let title = page.get_title().await.ok().flatten().unwrap_or_default();

    // Count console errors
    let console_errors = page
        .evaluate(
            "(function(){ \
                if(!window.__chromey_errs){window.__chromey_errs=[];const o=console.error;console.error=function(){window.__chromey_errs.push(Array.from(arguments).map(String).join(' '));o.apply(console,arguments)};} \
                return window.__chromey_errs.length; \
            })()"
        )
        .await
        .ok()
        .and_then(|r| r.value().cloned())
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    reset_refs();
    let tree = page
        .get_full_ax_tree(Some(-1), None)
        .await
        .map_err(|e| format!("Snapshot failed: {e}"))?;

    let (snapshot_text, ref_map) = build_snapshot(&tree.nodes);

    let mut out = String::new();
    out.push_str("### Page\n");
    out.push_str(&format!("- URL: {url}\n"));
    out.push_str(&format!("- Title: {title}\n"));
    if console_errors > 0 {
        out.push_str(&format!("- Console: {console_errors} errors\n"));
    }
    out.push_str("\n### Snapshot\n");
    out.push_str(&snapshot_text);

    Ok((out, ref_map))
}
