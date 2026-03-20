//! Accessibility tree snapshot with LLM-friendly ref-based element identifiers.
//!
//! Produces output like:
//! ```text
//! - heading "Example Domain" [ref=e1] [level=1]
//! - paragraph [ref=e2]: This domain is for use in illustrative examples.
//! - link "More information..." [ref=e3] [url=https://www.iana.org/domains/example]
//! ```
//!
//! The `[ref=eN]` tags let the LLM refer back to elements for click/hover/fill.

use chromiumoxide::cdp::browser_protocol::accessibility::AxNode;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

static REF_COUNTER: AtomicUsize = AtomicUsize::new(1);

/// Reset the ref counter (call when navigating to a new page).
pub fn reset_refs() {
    REF_COUNTER.store(1, Ordering::SeqCst);
}

fn next_ref() -> String {
    let n = REF_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("e{n}")
}

/// Build a ref-based accessibility snapshot and a ref→backendNodeId map.
/// Returns (snapshot_text, ref_map).
pub fn build_snapshot(
    nodes: &[AxNode],
) -> (String, HashMap<String, i64>) {
    let mut output = String::new();
    let mut ref_map: HashMap<String, i64> = HashMap::new();

    // Build parent→children index
    let mut children_map: HashMap<String, Vec<usize>> = HashMap::new();
    let mut id_to_idx: HashMap<String, usize> = HashMap::new();
    for (i, node) in nodes.iter().enumerate() {
        id_to_idx.insert(node.node_id.inner().clone(), i);
        if let Some(pid) = &node.parent_id {
            children_map
                .entry(pid.inner().clone())
                .or_default()
                .push(i);
        }
    }

    // Find root(s): nodes without parent_id
    let roots: Vec<usize> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.parent_id.is_none() && !n.ignored)
        .map(|(i, _)| i)
        .collect();

    fn format_tree(
        nodes: &[AxNode],
        children_map: &HashMap<String, Vec<usize>>,
        idx: usize,
        depth: usize,
        output: &mut String,
        ref_map: &mut HashMap<String, i64>,
    ) {
        let node = &nodes[idx];
        if node.ignored {
            // Still recurse into children of ignored nodes
            if let Some(kids) = children_map.get(node.node_id.inner()) {
                for &kid_idx in kids {
                    format_tree(nodes, children_map, kid_idx, depth, output, ref_map);
                }
            }
            return;
        }

        let role = node
            .role
            .as_ref()
            .and_then(|v| v.value.as_ref())
            .and_then(|v| v.as_str())
            .unwrap_or("generic");

        let name = node
            .name
            .as_ref()
            .and_then(|v| v.value.as_ref())
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Skip noise nodes
        if role == "none" || role == "generic" {
            // Recurse into children directly (flatten generics)
            if let Some(kids) = children_map.get(node.node_id.inner()) {
                for &kid_idx in kids {
                    format_tree(nodes, children_map, kid_idx, depth, output, ref_map);
                }
            }
            return;
        }

        // Skip pure StaticText nodes that duplicate their parent's name
        if role == "StaticText" {
            if let Some(kids) = children_map.get(node.node_id.inner()) {
                for &kid_idx in kids {
                    format_tree(nodes, children_map, kid_idx, depth, output, ref_map);
                }
            }
            return;
        }

        let indent = "  ".repeat(depth);
        let r = next_ref();

        // Store ref → backend_node_id mapping
        if let Some(backend_id) = node.backend_dom_node_id {
            ref_map.insert(r.clone(), *backend_id.inner());
        }

        // Build the line
        output.push_str(&format!("{indent}- {role}"));
        if !name.is_empty() {
            output.push_str(&format!(" \"{}\"", name));
        }
        output.push_str(&format!(" [ref={r}]"));

        // Add useful properties inline
        if let Some(props) = &node.properties {
            for prop in props {
                let pname = format!("{:?}", prop.name).to_lowercase();
                if let Some(val) = &prop.value.value {
                    match pname.as_str() {
                        "level" => output.push_str(&format!(" [level={val}]")),
                        "url" => output.push_str(&format!(" [url={val}]")),
                        "checked" => output.push_str(&format!(" [checked={val}]")),
                        "selected" => output.push_str(&format!(" [selected={val}]")),
                        "expanded" => output.push_str(&format!(" [expanded={val}]")),
                        "disabled" => {
                            if val.as_bool() == Some(true) {
                                output.push_str(" [disabled]");
                            }
                        }
                        "required" => {
                            if val.as_bool() == Some(true) {
                                output.push_str(" [required]");
                            }
                        }
                        "focused" => {
                            if val.as_bool() == Some(true) {
                                output.push_str(" [focused]");
                            }
                        }
                        "readonly" => {
                            if val.as_bool() == Some(true) {
                                output.push_str(" [readonly]");
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // Add description inline if different from name
        let desc = node
            .description
            .as_ref()
            .and_then(|v| v.value.as_ref())
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !desc.is_empty() && desc != name {
            output.push_str(&format!(": {desc}"));
        }

        output.push('\n');

        // Recurse children
        if let Some(kids) = children_map.get(node.node_id.inner()) {
            for &kid_idx in kids {
                format_tree(nodes, children_map, kid_idx, depth + 1, output, ref_map);
            }
        }
    }

    for root_idx in roots {
        format_tree(nodes, &children_map, root_idx, 0, &mut output, &mut ref_map);
    }

    (output, ref_map)
}

/// Build a page context + snapshot string for LLM responses.
pub async fn page_context_with_snapshot(
    page: &chromiumoxide::Page,
) -> Result<(String, HashMap<String, i64>), String> {
    let url = page.url().await.ok().flatten().unwrap_or_default();
    let title = page.get_title().await.ok().flatten().unwrap_or_default();

    // Get console errors via JS
    let console_info = page
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
    if console_info > 0 {
        out.push_str(&format!("- Console: {console_info} errors\n"));
    }
    out.push_str("\n### Snapshot\n");
    out.push_str(&snapshot_text);

    Ok((out, ref_map))
}
