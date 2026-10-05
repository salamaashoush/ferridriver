Feature: Interaction
  Browser interaction operations: click, fill, type, hover, focus.

  Scenario: Click a link
    Given I navigate to "http://127.0.0.1:47831/example-domain.html"
    When I click "a"
    Then the URL should contain "domain-info.html"

  Scenario: Fill and check value
    Given I navigate to "/input/textarea.html"
    When I fill "textarea" with "ferridriver"
    Then "textarea" should have value "ferridriver"

  Scenario: Check element visibility
    Given I navigate to "http://127.0.0.1:47831/example-domain.html"
    Then "h1" should be visible
    And "h1" should contain text "Example Domain"
