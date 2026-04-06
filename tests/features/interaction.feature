Feature: Interaction
  Browser interaction operations: click, fill, type, hover, focus.

  Scenario: Click a link
    Given I navigate to "https://example.com"
    When I click "a"
    Then the URL should contain "iana.org"

  Scenario: Fill and check value
    Given I navigate to "https://www.google.com"
    When I fill "textarea[name=q]" with "ferridriver"
    Then "textarea[name=q]" should have value "ferridriver"

  Scenario: Check element visibility
    Given I navigate to "https://example.com"
    Then "h1" should be visible
    And "h1" should contain text "Example Domain"
