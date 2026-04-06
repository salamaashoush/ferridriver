Feature: But Keyword
  The But keyword is a negative continuation, working like And.

  Scenario: Use But for negative assertions
    Given I navigate to "https://example.com"
    Then "h1" should be visible
    But "h1" should not contain text "Missing Content"
    And the page title should contain "Example"

  Scenario: Mix And and But keywords
    Given I navigate to "https://example.com"
    Then the page title should be "Example Domain"
    And "h1" should have text "Example Domain"
    But "h1" should not contain text "Google"
