//! Scenario execution model: expansion, variable interpolation, results.

use std::path::PathBuf;
use std::time::Duration;

use rustc_hash::FxHashMap;

use crate::feature::{ParsedFeature, SourceTag, extract_tags};

/// A concrete scenario ready for execution (after Outline expansion).
#[derive(Debug, Clone)]
pub struct ScenarioExecution {
  /// Parent feature name.
  pub feature_name: String,
  /// Feature file path.
  pub feature_path: PathBuf,
  /// The scenario's own title: its name, or — for one row of a Scenario
  /// Outline — the row's built title (see [`ExamplesTitle`]).
  pub name: String,
  /// The suites this scenario sits under, between the feature and its
  /// own title: a `Rule`'s name, and a Scenario Outline's name (which
  /// `playwright-bdd` renders as a `describe` around its rows).
  pub describe_path: Vec<String>,
  /// Merged tags (feature + rule + scenario + example tags).
  pub tags: Vec<String>,
  /// Steps to execute (Background steps prepended).
  pub steps: Vec<ScenarioStep>,
  /// Source location: `file:line`.
  pub location: String,
  /// Example values from Scenario Outline expansion.
  pub example_values: Option<FxHashMap<String, String>>,
  /// What a cucumber-json document needs and the execution model does
  /// not otherwise carry.
  pub source: ScenarioSource,
}

/// The Gherkin facts a report quotes but a run never reads.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioSource {
  pub feature_keyword: String,
  pub feature_name: String,
  pub feature_description: String,
  pub feature_line: usize,
  pub feature_tags: Vec<SourceTag>,
  pub rule_name: Option<String>,
  pub scenario_keyword: String,
  pub scenario_description: String,
  /// Cucumber's `element.line`: the row's line for a Scenario Outline,
  /// the scenario's own otherwise.
  pub scenario_line: usize,
  /// Every tag on this scenario, with the line it was written on —
  /// Cucumber's pickle tags: feature, rule, scenario and Examples.
  pub tags: Vec<SourceTag>,
}

/// A step within a scenario, extracted from the Gherkin AST.
#[derive(Debug, Clone)]
pub struct ScenarioStep {
  /// Keyword (Given, When, Then, And, But).
  pub keyword: String,
  /// Step text body (after keyword).
  pub text: String,
  /// Optional data table.
  pub table: Option<crate::data_table::DataTable>,
  /// Optional doc string.
  pub docstring: Option<String>,
  /// Line number in the feature file.
  pub line: usize,
  /// Line the step's docstring opens on, for a report that quotes it.
  pub docstring_line: usize,
}

impl ScenarioStep {
  /// Cucumber's `step.arguments`: a data table as `{rows:[{cells}]}`, a
  /// docstring as `{content, line}`, and an empty list for a bare step.
  #[must_use]
  pub fn cucumber_arguments(&self) -> serde_json::Value {
    if let Some(table) = &self.table {
      let rows: Vec<serde_json::Value> = table
        .raw()
        .iter()
        .map(|row| serde_json::json!({ "cells": row }))
        .collect();
      return serde_json::json!([{ "rows": rows }]);
    }
    if let Some(docstring) = &self.docstring {
      return serde_json::json!([{ "content": docstring, "line": self.docstring_line }]);
    }
    serde_json::json!([])
  }
}

/// How a Scenario Outline row is titled.
///
/// Ported from `playwright-bdd`'s
/// `src/generate/examplesTitleBuilder.ts`: the first of a
/// `# title-format:` comment above the Examples (or above its first
/// tag), the Examples' own name, the scenario's name — the last two
/// only when they name at least one Examples column — the configured
/// format, and finally `Example #<_index_>`. `_index_` counts rows
/// across EVERY Examples block of the scenario, not within one.
struct ExamplesTitle<'a> {
  parsed: &'a ParsedFeature,
  scenario_name: &'a str,
  configured: Option<&'a str>,
  index: usize,
}

impl<'a> ExamplesTitle<'a> {
  fn new(parsed: &'a ParsedFeature, scenario_name: &'a str, configured: Option<&'a str>) -> Self {
    Self {
      parsed,
      scenario_name,
      configured,
      index: 0,
    }
  }

  fn build(&mut self, examples: &gherkin::Examples, headers: &[String], row: &[String]) -> String {
    self.index += 1;
    let template = self.template(examples, headers);
    let mut params: FxHashMap<String, String> = FxHashMap::default();
    params.insert("_index_".to_string(), self.index.to_string());
    for (i, header) in headers.iter().enumerate() {
      if let Some(value) = row.get(i) {
        params.insert(header.clone(), value.clone());
      }
    }
    fill_template(&template, &params)
  }

  fn template(&self, examples: &gherkin::Examples, headers: &[String]) -> String {
    if let Some(from_comment) = self.title_comment(examples) {
      return from_comment;
    }
    if let Some(name) = examples.name.as_deref()
      && names_a_column(name, headers)
    {
      return name.to_string();
    }
    if names_a_column(self.scenario_name, headers) {
      return self.scenario_name.to_string();
    }
    if let Some(configured) = self.configured
      && !configured.is_empty()
    {
      return configured.to_string();
    }
    // English uses the singular and no colon; every other language
    // keeps its own Examples keyword.
    if examples.keyword.trim() == "Examples" || examples.keyword.trim() == "Scenarios" {
      "Example #<_index_>".to_string()
    } else {
      format!("{}: #<_index_>", examples.keyword.trim())
    }
  }

  /// The `# title-format:` comment directly above the Examples, or
  /// above its first tag. The one closest to Examples wins.
  fn title_comment(&self, examples: &gherkin::Examples) -> Option<String> {
    const PREFIX: &str = "# title-format:";
    let mut candidates = vec![examples.position.line.saturating_sub(1)];
    // The AST does not place tags, so the run of tag lines above the
    // keyword is walked instead: the comment above the FIRST of them is
    // the second candidate.
    let mut line = examples.position.line.saturating_sub(1);
    while line >= 1
      && let Some(text) = self.parsed.line(line).map(str::trim)
    {
      if text.starts_with('@') {
        candidates.push(line.saturating_sub(1));
      } else if !text.is_empty() && !text.starts_with('#') {
        break;
      }
      line -= 1;
    }
    for candidate in candidates {
      if let Some(comment) = self.parsed.comment_at(candidate)
        && let Some(rest) = comment.strip_prefix(PREFIX)
      {
        return Some(rest.trim().to_string());
      }
    }
    None
  }
}

/// Whether `text` is an Examples title template: it names at least one
/// of the block's columns in `<>`.
fn names_a_column(text: &str, headers: &[String]) -> bool {
  template_params(text)
    .iter()
    .any(|param| headers.iter().any(|header| header == param))
}

/// `playwright-bdd`'s `GherkinTemplate.extractParams`: every `<...>`.
fn template_params(text: &str) -> Vec<String> {
  let mut params = Vec::new();
  let bytes: Vec<char> = text.chars().collect();
  let mut i = 0;
  while i < bytes.len() {
    if bytes[i] == '<'
      && let Some(close) = (i + 1..bytes.len()).find(|&j| bytes[j] == '>')
      && close > i + 1
    {
      params.push(bytes[i + 1..close].iter().collect());
      i = close + 1;
      continue;
    }
    i += 1;
  }
  params
}

/// `playwright-bdd`'s `GherkinTemplate.fill`: substitute every `<...>`
/// the params name, and leave one they do not exactly as written.
fn fill_template(template: &str, params: &FxHashMap<String, String>) -> String {
  let mut out = String::with_capacity(template.len());
  let chars: Vec<char> = template.chars().collect();
  let mut i = 0;
  while i < chars.len() {
    if chars[i] == '<'
      && let Some(close) = (i + 1..chars.len()).find(|&j| chars[j] == '>')
      && close > i + 1
    {
      let key: String = chars[i + 1..close].iter().collect();
      match params.get(&key) {
        Some(value) => out.push_str(value),
        None => {
          out.push('<');
          out.push_str(&key);
          out.push('>');
        },
      }
      i = close + 1;
      continue;
    }
    out.push(chars[i]);
    i += 1;
  }
  out
}

/// What the expansion needs from the run's configuration.
#[derive(Debug, Clone, Default)]
pub struct ExpandOptions {
  /// `[test].examplesTitleFormat` — `playwright-bdd`'s config-level
  /// fallback title for a Scenario Outline row.
  pub examples_title_format: Option<String>,
}

/// Expand a parsed feature into concrete scenarios.
///
/// - Background steps are prepended to every scenario, a Rule's own
///   Background after the feature's
/// - Scenario Outlines are expanded with each Examples row, under a
///   describe named for the outline
/// - Tags are merged (feature + rule + scenario + example)
pub fn expand_feature(feature: &ParsedFeature) -> Vec<ScenarioExecution> {
  expand_feature_with(feature, &ExpandOptions::default())
}

/// [`expand_feature`] under a run's configuration.
pub fn expand_feature_with(parsed: &ParsedFeature, options: &ExpandOptions) -> Vec<ScenarioExecution> {
  let feature = &parsed.feature;
  let feature_tags = extract_tags(&feature.tags);
  let feature_tag_refs = parsed.tag_lines(feature.position.line, &feature.tags);

  let background_steps: Vec<ScenarioStep> = feature
    .background
    .as_ref()
    .map(|bg| bg.steps.iter().map(gherkin_step_to_scenario_step).collect())
    .unwrap_or_default();

  let mut scenarios = Vec::new();
  let mut context = ExpandContext {
    parsed,
    options,
    feature_tags: &feature_tags,
    feature_tag_refs: &feature_tag_refs,
    background: &background_steps,
  };
  context.expand_scenarios(&feature.scenarios, None, &[], &mut scenarios);

  for rule in &feature.rules {
    let rule_background: Vec<ScenarioStep> = rule
      .background
      .as_ref()
      .map(|bg| bg.steps.iter().map(gherkin_step_to_scenario_step).collect())
      .unwrap_or_default();
    let rule_tags = extract_tags(&rule.tags);
    let rule_tag_refs = parsed.tag_lines(rule.position.line, &rule.tags);
    context.expand_scenarios(
      &rule.scenarios,
      Some(RuleContext {
        name: &rule.name,
        tags: &rule_tags,
        tag_refs: &rule_tag_refs,
        background: &rule_background,
      }),
      &rule_background,
      &mut scenarios,
    );
  }

  scenarios
}

struct RuleContext<'a> {
  name: &'a str,
  tags: &'a [String],
  tag_refs: &'a [SourceTag],
  #[allow(dead_code)]
  background: &'a [ScenarioStep],
}

struct ExpandContext<'a> {
  parsed: &'a ParsedFeature,
  options: &'a ExpandOptions,
  feature_tags: &'a [String],
  feature_tag_refs: &'a [SourceTag],
  background: &'a [ScenarioStep],
}

impl ExpandContext<'_> {
  fn source(&self, rule: Option<&RuleContext<'_>>, scenario: &gherkin::Scenario) -> ScenarioSource {
    let feature = &self.parsed.feature;
    ScenarioSource {
      feature_keyword: feature.keyword.clone(),
      feature_name: feature.name.clone(),
      feature_description: description(feature.description.as_deref()),
      feature_line: feature.position.line,
      feature_tags: self.feature_tag_refs.to_vec(),
      rule_name: rule.map(|r| r.name.to_string()),
      scenario_keyword: scenario.keyword.clone(),
      scenario_description: description(scenario.description.as_deref()),
      scenario_line: scenario.position.line,
      tags: Vec::new(),
    }
  }

  fn expand_scenarios(
    &mut self,
    scenarios: &[gherkin::Scenario],
    rule: Option<RuleContext<'_>>,
    rule_background: &[ScenarioStep],
    out: &mut Vec<ScenarioExecution>,
  ) {
    let parsed = self.parsed;
    let feature = &parsed.feature;
    let rule = rule.as_ref();
    let describe_prefix: Vec<String> = rule.map(|r| vec![r.name.to_string()]).unwrap_or_default();

    for scenario in scenarios {
      let scenario_tag_refs = parsed.tag_lines(scenario.position.line, &scenario.tags);
      let base_tags: Vec<String> = self
        .feature_tags
        .iter()
        .chain(rule.iter().flat_map(|r| r.tags.iter()))
        .chain(extract_tags(&scenario.tags).iter())
        .cloned()
        .collect();
      let base_tag_refs: Vec<SourceTag> = self
        .feature_tag_refs
        .iter()
        .chain(rule.iter().flat_map(|r| r.tag_refs.iter()))
        .chain(scenario_tag_refs.iter())
        .cloned()
        .collect();

      let mut steps = self.background.to_vec();
      steps.extend_from_slice(rule_background);

      if scenario.examples.is_empty() {
        let mut steps = steps;
        steps.extend(scenario.steps.iter().map(gherkin_step_to_scenario_step));
        let mut source = self.source(rule, scenario);
        source.tags = base_tag_refs;
        out.push(ScenarioExecution {
          feature_name: feature.name.clone(),
          feature_path: parsed.path.clone(),
          name: scenario.name.clone(),
          describe_path: describe_prefix.clone(),
          tags: base_tags,
          steps,
          location: format!("{}:{}", parsed.path.display(), scenario.position.line),
          example_values: None,
          source,
        });
        continue;
      }

      // A Scenario Outline is a describe named for the outline, with one
      // test per Examples row (`playwright-bdd`'s `renderScenarioOutline`).
      let mut describe = describe_prefix.clone();
      describe.push(scenario.name.clone());
      let mut titles = ExamplesTitle::new(parsed, &scenario.name, self.options.examples_title_format.as_deref());

      for example in &scenario.examples {
        let Some(table) = &example.table else { continue };
        if table.rows.len() < 2 {
          continue;
        }
        let example_tags = extract_tags(&example.tags);
        let example_tag_refs = parsed.tag_lines(example.position.line, &example.tags);
        let row_lines = parsed.table_row_lines(table.position.line, table.rows.len());
        let headers = &table.rows[0];

        for (row_idx, row) in table.rows[1..].iter().enumerate() {
          let mut values: FxHashMap<String, String> = FxHashMap::default();
          for (i, header) in headers.iter().enumerate() {
            if let Some(value) = row.get(i) {
              values.insert(header.clone(), value.clone());
            }
          }

          let mut row_steps = steps.clone();
          row_steps.extend(scenario.steps.iter().map(|s| {
            let mut step = gherkin_step_to_scenario_step(s);
            step.text = substitute_placeholders(&step.text, &values);
            if let Some(table) = &mut step.table {
              for row in table.iter_mut() {
                for cell in row.iter_mut() {
                  *cell = substitute_placeholders(cell, &values);
                }
              }
            }
            if let Some(ds) = &mut step.docstring {
              *ds = substitute_placeholders(ds, &values);
            }
            step
          }));

          let row_line = row_lines.get(row_idx + 1).copied().unwrap_or(scenario.position.line);
          let mut source = self.source(rule, scenario);
          source.scenario_line = row_line;
          source.tags = base_tag_refs
            .iter()
            .cloned()
            .chain(example_tag_refs.iter().cloned())
            .collect();

          out.push(ScenarioExecution {
            feature_name: feature.name.clone(),
            feature_path: parsed.path.clone(),
            name: titles.build(example, headers, row),
            describe_path: describe.clone(),
            tags: base_tags.iter().cloned().chain(example_tags.iter().cloned()).collect(),
            steps: row_steps,
            location: format!("{}:{}", parsed.path.display(), row_line),
            example_values: Some(values),
            source,
          });
        }
      }
    }
  }
}

fn description(text: Option<&str>) -> String {
  text.map(|d| d.trim_end().to_string()).unwrap_or_default()
}

fn gherkin_step_to_scenario_step(step: &gherkin::Step) -> ScenarioStep {
  ScenarioStep {
    keyword: step.keyword.clone(),
    text: step.value.clone(),
    table: step.table.as_ref().map(crate::feature::table_to_vec),
    docstring: step.docstring.clone(),
    line: step.position.line,
    // The AST places a docstring only by its step; the opening fence is
    // the next line a report can name.
    docstring_line: step.position.line + 1,
  }
}

fn substitute_placeholders(text: &str, values: &FxHashMap<String, String>) -> String {
  let mut result = text.to_string();
  for (key, val) in values {
    result = result.replace(&format!("<{key}>"), val);
  }
  result
}

// ── Scenario result types ──

/// Status of a single step execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum StepStatus {
  Passed,
  Failed,
  Skipped,
  Undefined,
  Pending,
}

/// Status of a scenario execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ScenarioStatus {
  Passed,
  Failed,
  Skipped,
  Undefined,
}

/// Result of executing a single step.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StepResult {
  pub keyword: String,
  pub text: String,
  pub status: StepStatus,
  pub duration: Duration,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub error: Option<String>,
}

/// Result of executing an entire scenario.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScenarioResult {
  pub feature_name: String,
  pub feature_path: String,
  pub scenario_name: String,
  pub status: ScenarioStatus,
  pub steps: Vec<StepResult>,
  pub duration: Duration,
  pub attempt: u32,
  pub tags: Vec<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub error: Option<String>,
  #[serde(skip)]
  pub failure_screenshot: Option<Vec<u8>>,
}

impl ScenarioResult {
  /// Whether this scenario should be retried.
  pub fn should_retry(&self, max_retries: u32) -> bool {
    self.status == ScenarioStatus::Failed && self.attempt < max_retries
  }
}
