Feature: Asterisk Keyword
  The * keyword can replace Given/When/Then/And/But for generic steps.

  Scenario: Use asterisk keyword for all steps
    * I navigate to "https://example.com"
    * the page title should contain "Example"
    * "h1" should be visible
    * "h1" should have text "Example Domain"

  Scenario: Mix asterisk with standard keywords
    Given I navigate to "https://example.com"
    * "h1" should be visible
    Then the page title should be "Example Domain"
    * the URL should contain "example.com"
