//! BDD scenario runner — parse Gherkin scripts and execute via the step registry.

use crate::steps::StepRegistry;
use base64::Engine;
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide::Page;
use std::collections::HashMap;
use std::time::Instant;

// ─── Types ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScenarioResult {
    pub scenario: Option<String>,
    pub status: String,
    pub total_duration_ms: u64,
    pub summary: String,
    pub steps: Vec<StepResult>,
    pub variables: HashMap<String, String>,
    #[serde(skip)]
    pub failure_screenshots: Vec<ScreenshotData>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StepResult {
    pub step: usize,
    pub keyword: String,
    pub description: String,
    pub status: String,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct ScreenshotData {
    pub step: usize,
    pub base64: String,
}

pub struct ScenarioOptions {
    pub stop_on_failure: bool,
    pub screenshot_on_failure: bool,
}

// ─── Parser ──────────────────────────────────────────────────────────────────

struct ParsedStep {
    keyword: String,
    body: String,
    data_table: Option<Vec<Vec<String>>>,
}

fn parse(script: &str) -> Result<(Option<String>, Vec<ParsedStep>), String> {
    let mut scenario_name = None;
    let mut steps = Vec::new();
    let lines: Vec<&str> = script.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i].trim();
        i += 1;

        if line.is_empty() || line.starts_with('#') || line.starts_with("Feature:") {
            continue;
        }
        if line.starts_with("Scenario:") {
            scenario_name = Some(line.strip_prefix("Scenario:").unwrap().trim().to_string());
            continue;
        }

        let (keyword, body) = if let Some(r) = line.strip_prefix("Given ") {
            ("Given", r)
        } else if let Some(r) = line.strip_prefix("When ") {
            ("When", r)
        } else if let Some(r) = line.strip_prefix("Then ") {
            ("Then", r)
        } else if let Some(r) = line.strip_prefix("And ") {
            ("And", r)
        } else if let Some(r) = line.strip_prefix("But ") {
            ("But", r)
        } else {
            return Err(format!("Line {i}: Expected Given/When/Then/And/But, got: '{line}'"));
        };

        // Collect data table rows (lines starting with |)
        let mut data_table = None;
        while i < lines.len() && lines[i].trim().starts_with('|') {
            let cells: Vec<String> = lines[i]
                .trim()
                .split('|')
                .filter(|s| !s.is_empty())
                .map(|s| s.trim().to_string())
                .collect();
            data_table.get_or_insert_with(Vec::new).push(cells);
            i += 1;
        }

        steps.push(ParsedStep {
            keyword: keyword.to_string(),
            body: body.to_string(),
            data_table,
        });
    }

    if steps.is_empty() {
        return Err("No steps found in script".to_string());
    }
    Ok((scenario_name, steps))
}

// ─── Variable Interpolation ──────────────────────────────────────────────────

/// Replace $var references with values. Skip $var after "as " (those are definitions).
fn interpolate(s: &str, vars: &HashMap<String, String>) -> String {
    // Don't interpolate the variable name in "as $varname" patterns
    let (interp_part, keep_part) = if let Some(idx) = s.find(" as $") {
        (&s[..idx], Some(&s[idx..]))
    } else {
        (s, None)
    };

    let mut result = String::with_capacity(interp_part.len());
    let mut chars = interp_part.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' {
            if chars.peek() == Some(&'$') {
                chars.next();
                result.push('$');
            } else {
                let mut name = String::new();
                while let Some(&nc) = chars.peek() {
                    if nc.is_alphanumeric() || nc == '_' {
                        name.push(nc);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if name.is_empty() {
                    result.push('$');
                } else if let Some(val) = vars.get(&name) {
                    result.push_str(val);
                } else {
                    // Keep unresolved — the step might handle it or it'll error naturally
                    result.push('$');
                    result.push_str(&name);
                }
            }
        } else {
            result.push(c);
        }
    }

    if let Some(suffix) = keep_part {
        result.push_str(suffix);
    }
    result
}

// ─── Runner ──────────────────────────────────────────────────────────────────

pub async fn run(
    page: &Page,
    script: &str,
    options: ScenarioOptions,
) -> Result<ScenarioResult, String> {
    let (scenario_name, steps) = parse(script)?;
    let registry = StepRegistry::global();

    let mut variables: HashMap<String, String> = HashMap::new();
    let mut results: Vec<StepResult> = Vec::new();
    let mut failure_screenshots: Vec<ScreenshotData> = Vec::new();
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;
    let mut should_skip = false;
    let total_start = Instant::now();

    for (idx, step) in steps.iter().enumerate() {
        let step_num = idx + 1;

        if should_skip {
            skipped += 1;
            results.push(StepResult {
                step: step_num,
                keyword: step.keyword.clone(),
                description: step.body.clone(),
                status: "skipped".into(),
                duration_ms: 0,
                error: None,
                data: None,
            });
            continue;
        }

        let body = interpolate(&step.body, &variables);
        let table = step.data_table.as_ref().map(|rows| {
            rows.iter()
                .map(|row| {
                    row.iter()
                        .map(|cell| interpolate(cell, &variables))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        });

        let t0 = Instant::now();
        let result = registry
            .execute(page, &body, table.as_deref(), &mut variables)
            .await;
        let duration_ms = t0.elapsed().as_millis() as u64;

        match result {
            Ok(data) => {
                passed += 1;
                results.push(StepResult {
                    step: step_num,
                    keyword: step.keyword.clone(),
                    description: body,
                    status: "passed".into(),
                    duration_ms,
                    error: None,
                    data,
                });
            }
            Err(err) => {
                failed += 1;

                if options.screenshot_on_failure {
                    if let Ok(bytes) =
                        page.screenshot(ScreenshotParams::builder().build()).await
                    {
                        failure_screenshots.push(ScreenshotData {
                            step: step_num,
                            base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
                        });
                    }
                }

                results.push(StepResult {
                    step: step_num,
                    keyword: step.keyword.clone(),
                    description: body,
                    status: "failed".into(),
                    duration_ms,
                    error: Some(err),
                    data: None,
                });

                if options.stop_on_failure {
                    should_skip = true;
                }
            }
        }
    }

    let total_duration_ms = total_start.elapsed().as_millis() as u64;
    let status = if failed == 0 { "passed" } else { "failed" }.to_string();
    let summary = format!("{passed} passed, {failed} failed, {skipped} skipped in {total_duration_ms}ms");

    Ok(ScenarioResult {
        scenario: scenario_name,
        status,
        total_duration_ms,
        summary,
        steps: results,
        variables,
        failure_screenshots,
    })
}
