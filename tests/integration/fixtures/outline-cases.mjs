export default [
  {
    "name": "rows_default_to_example_index_running_across_blocks",
    "source": "Feature: Sites\n\n  Scenario Outline: Visit\n    Given I visit <url>\n\n    Examples: Popular\n      | url |\n      | a   |\n      | b   |\n\n    Examples: Others\n      | url |\n      | c   |\n"
  },
  {
    "name": "an_examples_name_that_names_a_column_is_the_title",
    "source": "Feature: Sites\n\n  Scenario Outline: Visit\n    Given I visit <url>\n\n    Examples: visiting <url>\n      | url |\n      | a   |\n      | b   |\n"
  },
  {
    "name": "a_scenario_name_that_names_a_column_is_the_title",
    "source": "Feature: Sites\n\n  Scenario Outline: user <name> is <age>\n    Given nothing\n\n    Examples: people\n      | name  | age |\n      | sashoush   | 36  |\n"
  },
  {
    "name": "a_title_format_comment_above_examples_wins",
    "source": "Feature: Sites\n\n  Scenario Outline: Visit\n    Given I visit <url>\n\n    # title-format: hitting <url> (<_index_>)\n    Examples: visiting <url>\n      | url |\n      | a   |\n"
  },
  {
    "name": "a_title_format_comment_above_the_tags_is_found",
    "source": "Feature: Sites\n\n  Scenario Outline: Visit\n    Given I visit <url>\n\n    # title-format: tagged <url>\n    @smoke\n    Examples:\n      | url |\n      | a   |\n"
  },
  {
    "name": "the_configured_format_is_the_last_fallback",
    "source": "Feature: Sites\n\n  Scenario Outline: Visit\n    Given I visit <url>\n\n    Examples:\n      | url |\n      | a   |\n",
    "titleFormat": "row <_index_>: <url>"
  },
  {
    "name": "an_unknown_placeholder_is_left_as_written",
    "source": "Feature: Sites\n\n  Scenario Outline: Visit\n    Given I visit <url>\n\n    Examples:\n      | url |\n      | a   |\n",
    "titleFormat": "<url> and <missing>"
  },
  {
    "name": "a_rule_expands_its_outlines_and_merges_its_tags",
    "source": "@feature\nFeature: Rules\n\n  Background:\n    Given a page\n\n  @rule\n  Rule: Structure\n\n    Background:\n      Given a rule background\n\n    @scenario\n    Scenario Outline: Check <thing>\n      Then <thing> is visible\n\n      Examples:\n        | thing |\n        | h1    |\n        | p     |\n"
  },
  {
    "name": "the_source_carries_what_a_report_quotes",
    "source": "@one @two\nFeature: Sites\n  The feature's description.\n\n  @three\n  Scenario Outline: Visit\n    The scenario's description.\n\n    Given I visit <url>\n\n    @four\n    Examples:\n      | url |\n      | a   |\n      | b   |\n"
  },
  {
    "name": "a_plain_scenario_keeps_its_name_and_has_no_describe",
    "source": "Feature: Sites\n\n  Scenario: Visit the page\n    Given a page\n"
  }
];
