Feature: Background Steps

  Background:
    Given I navigate to "https://example.com"

  Scenario: Background provides page for title check
    Then the page title should be "Example Domain"

  Scenario: Background provides page for element check
    Then "h1" should have text "Example Domain"
    And "h1" should be visible

  Scenario: Background provides page for URL check
    Then the URL should contain "example.com"
