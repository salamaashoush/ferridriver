Feature: Navigation
  Basic browser navigation operations.

  Scenario: Navigate to a page
    Given I navigate to "https://example.com"
    Then the page title should contain "Example"
    And the URL should contain "example.com"

  Scenario: Navigate and check URL
    Given I navigate to "https://example.com"
    Then the URL should be "https://example.com/"

  Scenario: Reload page
    Given I navigate to "https://example.com"
    When I reload the page
    Then the page title should contain "Example"
