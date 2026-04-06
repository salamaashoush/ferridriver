@regression
Feature: Tag Expressions
  Complex tag expression filtering with and/or/not/parentheses.

  @smoke @fast
  Scenario: Double-tagged scenario
    Given I navigate to "https://example.com"
    Then "h1" should be visible

  @slow
  Scenario: Slow scenario
    Given I navigate to "https://example.com"
    When I wait 1 seconds
    Then the page title should be "Example Domain"

  @wip
  Scenario: Work in progress
    Given I navigate to "https://example.com"
    Then "h1" should be visible

  @api @fast
  Scenario: API tagged scenario
    Given I navigate to "https://example.com"
    Then the URL should contain "example"
