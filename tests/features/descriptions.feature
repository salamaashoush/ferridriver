Feature: Feature and Scenario Descriptions
  This feature tests that free-form description text is properly
  parsed and ignored by the runner. Descriptions can span
  multiple lines after Feature, Scenario, or Rule keywords.

  Scenario: Scenario with a description
    This is a multi-line description for the scenario.
    It should be ignored by the step matcher.

    Given I navigate to "https://example.com"
    Then the page title should be "Example Domain"

  Rule: Rules can have descriptions too
    This rule groups scenarios about page structure.
    The description is informational only.

    Scenario: Rule scenario with description
      This scenario validates the heading element.

      Given I navigate to "https://example.com"
      Then "h1" should be visible
