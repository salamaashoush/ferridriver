Feature: Navigation
  Basic browser navigation operations.

  Scenario: Navigate to a page
    Given I navigate to "http://127.0.0.1:47831/example-domain.html"
    Then the page title should contain "Example"
    And the URL should contain "example-domain.html"

  Scenario: Navigate and check URL
    Given I navigate to "http://127.0.0.1:47831/example-domain.html"
    Then the URL should be "http://127.0.0.1:47831/example-domain.html"

  Scenario: Reload page
    Given I navigate to "http://127.0.0.1:47831/example-domain.html"
    When I reload the page
    Then the page title should contain "Example"
