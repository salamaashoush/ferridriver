//! Accessibility tree snapshot — compact, LLM-friendly format.

use crate::backend::{AnyPage, AxNodeData};
use rustc_hash::FxHashMap as HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

static REF_COUNTER: AtomicUsize = AtomicUsize::new(1);

pub fn reset_refs() {
    REF_COUNTER.store(1, Ordering::SeqCst);
}

fn next_ref() -> String {
    format!("e{}", REF_COUNTER.fetch_add(1, Ordering::SeqCst))
}

const NOISE_ROLES: &[&str] = &[
    "none", "generic", "InlineTextBox", "LineBreak",
    "LayoutTable", "LayoutTableRow", "LayoutTableCell",
    "LayoutTableColumn", "LayoutTableBody",
];

const INTERACTIVE_ROLES: &[&str] = &[
    "link", "button", "textbox", "checkbox", "radio", "combobox",
    "menuitem", "tab", "switch", "slider", "spinbutton",
    "searchbox", "option", "menuitemcheckbox", "menuitemradio",
];

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

const MAX_SNAPSHOT_CHARS: usize = 15_000;
const MAX_TEXT_LEN: usize = 80;

/// Build a compact snapshot. Returns (text, ref_map).
pub fn build_snapshot(nodes: &[AxNodeData]) -> (String, HashMap<String, i64>) {
    let mut output = String::with_capacity(nodes.len() * 64);
    let mut ref_map: HashMap<String, i64> = HashMap::default();
    let mut truncated = false;

    let mut children_map: HashMap<&str, Vec<usize>> = HashMap::default();
    for (i, node) in nodes.iter().enumerate() {
        if let Some(pid) = &node.parent_id {
            children_map.entry(pid.as_str()).or_default().push(i);
        }
    }

    let roots: Vec<usize> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.parent_id.is_none() && !n.ignored)
        .map(|(i, _)| i)
        .collect();

    fn get_role(node: &AxNodeData) -> &str {
        node.role.as_deref().unwrap_or("generic")
    }

    fn get_name(node: &AxNodeData) -> &str {
        node.name.as_deref().unwrap_or("")
    }

    fn get_desc(node: &AxNodeData) -> &str {
        node.description.as_deref().unwrap_or("")
    }

    fn format_tree(
        nodes: &[AxNodeData],
        children_map: &HashMap<&str, Vec<usize>>,
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

        if is_noise(role) {
            recurse_children(nodes, children_map, idx, depth, output, ref_map, truncated);
            return;
        }

        if role == "StaticText" {
            return;
        }

        if role == "RootWebArea" {
            recurse_children(nodes, children_map, idx, depth, output, ref_map, truncated);
            return;
        }

        if !is_interactive(role) && !SEMANTIC_ROLES.contains(&role) && name.is_empty() {
            recurse_children(nodes, children_map, idx, depth, output, ref_map, truncated);
            return;
        }

        let indent = "  ".repeat(depth);

        let ref_str = if needs_ref(role) || is_interactive(role) {
            let r = next_ref();
            if let Some(bid) = node.backend_dom_node_id {
                ref_map.insert(r.clone(), bid);
            }
            format!(" [ref={r}]")
        } else {
            String::new()
        };

        use std::fmt::Write;
        let _ = write!(output, "{indent}- {role}");

        if !name.is_empty() {
            if name.len() > MAX_TEXT_LEN {
                let _ = write!(output, " \"{}...\"", &name[..MAX_TEXT_LEN]);
            } else {
                let _ = write!(output, " \"{name}\"");
            }
        }

        output.push_str(&ref_str);

        for prop in &node.properties {
            if let Some(val) = &prop.value {
                match prop.name.as_str() {
                    "level" => { let _ = write!(output, " [level={val}]"); }
                    "url" if is_interactive(role) => {
                        let u = val.as_str().unwrap_or("");
                        if !u.is_empty() && u.len() <= 100 {
                            let _ = write!(output, " [url={u}]");
                        }
                    }
                    "checked" if val.as_bool() == Some(true) => output.push_str(" [checked]"),
                    "selected" if val.as_bool() == Some(true) => output.push_str(" [selected]"),
                    "expanded" => { let _ = write!(output, " [expanded={val}]"); }
                    "disabled" if val.as_bool() == Some(true) => output.push_str(" [disabled]"),
                    "required" if val.as_bool() == Some(true) => output.push_str(" [required]"),
                    "focused" if val.as_bool() == Some(true) => output.push_str(" [focused]"),
                    "readonly" if val.as_bool() == Some(true) => output.push_str(" [readonly]"),
                    _ => {}
                }
            }
        }

        if matches!(role, "textbox" | "combobox" | "searchbox" | "spinbutton") {
            for prop in &node.properties {
                if prop.name == "value" {
                    if let Some(val) = &prop.value {
                        if let Some(s) = val.as_str() {
                            if !s.is_empty() {
                                let display_val = if s.len() > 50 { &s[..50] } else { s };
                                output.push_str(&format!(" [value=\"{display_val}\"]"));
                            }
                        }
                    }
                    break;
                }
            }
        }

        if !desc.is_empty() && desc != name {
            let d = if desc.len() > MAX_TEXT_LEN {
                &desc[..MAX_TEXT_LEN]
            } else {
                desc
            };
            output.push_str(&format!(": {d}"));
        }

        output.push('\n');

        recurse_children(nodes, children_map, idx, depth + 1, output, ref_map, truncated);
    }

    fn recurse_children(
        nodes: &[AxNodeData],
        children_map: &HashMap<&str, Vec<usize>>,
        idx: usize,
        depth: usize,
        output: &mut String,
        ref_map: &mut HashMap<String, i64>,
        truncated: &mut bool,
    ) {
        if let Some(kids) = children_map.get(nodes[idx].node_id.as_str()) {
            for &kid_idx in kids {
                format_tree(nodes, children_map, kid_idx, depth, output, ref_map, truncated);
            }
        }
    }

    for root_idx in roots {
        format_tree(
            nodes,
            &children_map,
            root_idx,
            0,
            &mut output,
            &mut ref_map,
            &mut truncated,
        );
    }

    (output, ref_map)
}

/// Build page context header + snapshot.
pub async fn page_context_with_snapshot(
    page: &AnyPage,
) -> Result<(String, HashMap<String, i64>), String> {
    let url = page.url().await.ok().flatten().unwrap_or_default();
    let title = page.title().await.ok().flatten().unwrap_or_default();

    let console_errors = crate::actions::console_error_count(page).await;

    reset_refs();
    let tree = page.accessibility_tree().await?;
    let (snapshot_text, ref_map) = build_snapshot(&tree);

    let mut out = String::new();
    out.push_str("### Page\n");
    out.push_str(&format!("- URL: {url}\n"));
    out.push_str(&format!("- Title: {title}\n"));
    if console_errors > 0 {
        out.push_str(&format!("- Console: {console_errors} errors\n"));
    }

    if let Ok(si) = crate::actions::scroll_info(page).await {
        if si.scroll_height > 0 {
            out.push_str(&format!(
                "- Scroll: {}/{}px (viewport: {}px)\n",
                si.scroll_y, si.scroll_height, si.viewport_height
            ));
        }
    }

    out.push_str("\n### Snapshot\n");
    out.push_str(&snapshot_text);

    Ok((out, ref_map))
}
