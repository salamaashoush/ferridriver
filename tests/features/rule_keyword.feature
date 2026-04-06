Feature: Rule Keyword
  Gherkin 6+ Rule keyword groups related scenarios under a business rule.

  Background:
    Given I navigate to "https://example.com"

  Rule: Page structure
    Scenario: Has a heading
      Then "h1" should be visible
      And "h1" should have text "Example Domain"

    Scenario: Has paragraphs
      Then there should be 2 "p" elements

  Rule: Page metadata
    Scenario: Has correct title
      Then the page title should be "Example Domain"

    Scenario: Has correct URL
      Then the URL should contain "example.com"
