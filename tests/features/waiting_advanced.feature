Feature: Auto-waiting
  Tests that elements become visible, enabled, or change text over time.

  Scenario: Wait for delayed element to appear
    Given I navigate to "/waiting.html"
    When I click "#add-delayed"
    Then "#delayed" should be visible

  Scenario: Wait for hidden element to become visible
    Given I navigate to "/waiting.html"
    When I click "#add-hidden"
    Then "#will-show" should be visible

  Scenario: Wait for text to change
    Given I navigate to "/waiting.html"
    Then "#changing-text" should contain text "Ready!"

  Scenario: Disabled input becomes enabled
    Given I navigate to "/waiting.html"
    When I click "#enable-input"
    Then "#disabled-input" should be enabled
