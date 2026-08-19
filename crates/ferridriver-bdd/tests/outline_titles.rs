#![allow(clippy::expect_used, clippy::unwrap_used)]
//! How a Scenario Outline row is titled, and what a `Rule` holds.
//!
//! Ported from `playwright-bdd`'s
//! `src/generate/examplesTitleBuilder.ts`, so a suite moved off it
//! keeps the titles its `--grep`, its last-failed list and its
//! cucumber-json `element.name` are written against.

use ferridriver_bdd::feature::FeatureSet;
use ferridriver_bdd::scenario::{ExpandOptions, ScenarioExecution, expand_feature, expand_feature_with};

fn expand(text: &str) -> Vec<ScenarioExecution> {
  let set = FeatureSet::parse_text(text).expect("parses");
  expand_feature(&set.features[0])
}

fn expand_with(text: &str, options: &ExpandOptions) -> Vec<ScenarioExecution> {
  let set = FeatureSet::parse_text(text).expect("parses");
  expand_feature_with(&set.features[0], options)
}

fn titles(scenarios: &[ScenarioExecution]) -> Vec<&str> {
  scenarios.iter().map(|s| s.name.as_str()).collect()
}

#[test]
fn rows_default_to_example_index_running_across_blocks() {
  let scenarios = expand(
    r#"Feature: Sites

  Scenario Outline: Visit
    Given I visit <url>

    Examples: Popular
      | url |
      | a   |
      | b   |

    Examples: Others
      | url |
      | c   |
"#,
  );
  assert_eq!(
    titles(&scenarios),
    vec!["Example #1", "Example #2", "Example #3"],
    "the index counts rows across every Examples block, not within one",
  );
  // The outline itself is the describe its rows sit under.
  assert_eq!(scenarios[0].describe_path, vec!["Visit".to_string()]);
}

#[test]
fn an_examples_name_that_names_a_column_is_the_title() {
  let scenarios = expand(
    r#"Feature: Sites

  Scenario Outline: Visit
    Given I visit <url>

    Examples: visiting <url>
      | url |
      | a   |
      | b   |
"#,
  );
  assert_eq!(titles(&scenarios), vec!["visiting a", "visiting b"]);
}

#[test]
fn a_scenario_name_that_names_a_column_is_the_title() {
  let scenarios = expand(
    r#"Feature: Sites

  Scenario Outline: user <name> is <age>
    Given nothing

    Examples: people
      | name  | age |
      | Ada   | 36  |
"#,
  );
  assert_eq!(titles(&scenarios), vec!["user Ada is 36"]);
  assert_eq!(
    scenarios[0].describe_path,
    vec!["user <name> is <age>".to_string()],
    "the describe keeps the outline's own name, unfilled",
  );
}

#[test]
fn a_title_format_comment_above_examples_wins() {
  let scenarios = expand(
    r#"Feature: Sites

  Scenario Outline: Visit
    Given I visit <url>

    # title-format: hitting <url> (<_index_>)
    Examples: visiting <url>
      | url |
      | a   |
"#,
  );
  assert_eq!(titles(&scenarios), vec!["hitting a (1)"]);
}

#[test]
fn a_title_format_comment_above_the_tags_is_found() {
  let scenarios = expand(
    r#"Feature: Sites

  Scenario Outline: Visit
    Given I visit <url>

    # title-format: tagged <url>
    @smoke
    Examples:
      | url |
      | a   |
"#,
  );
  assert_eq!(titles(&scenarios), vec!["tagged a"]);
}

#[test]
fn the_configured_format_is_the_last_fallback() {
  let text = r#"Feature: Sites

  Scenario Outline: Visit
    Given I visit <url>

    Examples:
      | url |
      | a   |
"#;
  let scenarios = expand_with(
    text,
    &ExpandOptions {
      examples_title_format: Some("row <_index_>: <url>".to_string()),
      ..Default::default()
    },
  );
  assert_eq!(titles(&scenarios), vec!["row 1: a"]);
}

#[test]
fn an_unknown_placeholder_is_left_as_written() {
  let scenarios = expand_with(
    r#"Feature: Sites

  Scenario Outline: Visit
    Given I visit <url>

    Examples:
      | url |
      | a   |
"#,
    &ExpandOptions {
      examples_title_format: Some("<url> and <missing>".to_string()),
      ..Default::default()
    },
  );
  assert_eq!(titles(&scenarios), vec!["a and <missing>"]);
}

#[test]
fn a_rule_expands_its_outlines_and_merges_its_tags() {
  let scenarios = expand(
    r#"@feature
Feature: Rules

  Background:
    Given a page

  @rule
  Rule: Structure

    Background:
      Given a rule background

    @scenario
    Scenario Outline: Check <thing>
      Then <thing> is visible

      Examples:
        | thing |
        | h1    |
        | p     |
"#,
  );
  assert_eq!(
    titles(&scenarios),
    vec!["Check h1", "Check p"],
    "an outline inside a Rule expands — it used to be dropped, one scenario per outline",
  );
  assert_eq!(
    scenarios[0].describe_path,
    vec!["Structure".to_string(), "Check <thing>".to_string()],
    "a Rule is a describe, and the outline another inside it",
  );
  assert!(
    scenarios[0].tags.contains(&"@rule".to_string()),
    "the Rule's own tags reach its scenarios: {:?}",
    scenarios[0].tags
  );
  assert!(scenarios[0].tags.contains(&"@feature".to_string()));
  assert!(scenarios[0].tags.contains(&"@scenario".to_string()));
  assert_eq!(
    scenarios[0].steps.len(),
    3,
    "feature background, rule background, then the step",
  );
  assert_eq!(scenarios[0].source.rule_name.as_deref(), Some("Structure"));
}

#[test]
fn the_source_carries_what_a_report_quotes() {
  let scenarios = expand(
    r#"@one @two
Feature: Sites
  The feature's description.

  @three
  Scenario Outline: Visit
    The scenario's description.

    Given I visit <url>

    @four
    Examples:
      | url |
      | a   |
      | b   |
"#,
  );
  let source = &scenarios[0].source;
  assert_eq!(source.feature_keyword, "Feature");
  assert_eq!(source.feature_name, "Sites");
  assert!(
    source.feature_description.contains("The feature's description."),
    "{:?}",
    source.feature_description
  );
  assert_eq!(source.feature_line, 2, "the Feature keyword's own line");
  assert_eq!(source.scenario_keyword, "Scenario Outline");
  assert!(source.scenario_description.contains("The scenario's description."));

  let names: Vec<&str> = source.tags.iter().map(|t| t.name.as_str()).collect();
  assert_eq!(names, vec!["@one", "@two", "@three", "@four"]);
  assert!(
    source.tags.iter().all(|tag| tag.line > 0),
    "every tag is placed on the line it was written: {:?}",
    source.tags
  );
  assert_eq!(source.tags[0].line, source.tags[1].line, "@one @two share a line");

  // Cucumber's `element.line` for an outline row is the ROW's line, and
  // two rows differ by one.
  let first = scenarios[0].source.scenario_line;
  let second = scenarios[1].source.scenario_line;
  assert_eq!(
    second,
    first + 1,
    "row lines are read off the source: {first} then {second}"
  );
}

#[test]
fn a_plain_scenario_keeps_its_name_and_has_no_describe() {
  let scenarios = expand(
    r#"Feature: Sites

  Scenario: Visit the page
    Given a page
"#,
  );
  assert_eq!(titles(&scenarios), vec!["Visit the page"]);
  assert!(scenarios[0].describe_path.is_empty());
  assert_eq!(scenarios[0].source.scenario_keyword, "Scenario");
}
