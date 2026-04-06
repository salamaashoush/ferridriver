Feature: Variables and JavaScript
  Variable storage and JavaScript evaluation.

  Scenario: Store and use variable
    Given I navigate to "https://example.com"
    When I store the text of "h1" as "heading"
    Then I set variable "expected" to "Example Domain"

  Scenario: Evaluate JavaScript
    Given I navigate to "https://example.com"
    When I evaluate "document.title"
    Then the page title should be "Example Domain"

  Scenario: Store JavaScript result
    Given I navigate to "https://example.com"
    When I store the result of "document.querySelectorAll('p').length.toString()" as "count"
