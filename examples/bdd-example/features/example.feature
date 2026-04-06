Feature: Example Domain
  Test the example.com page using both built-in and custom steps.

  Scenario: Page loads correctly
    Given I am on the example page
    Then I should see the example heading
    And the page title should be "Example Domain"
    And the URL should contain "example.com"

  Scenario: Store and verify page info
    Given I am on the example page
    When I store the page info
    Then "h1" should be visible
    And "h1" should contain text "Example"

  Scenario: Element assertions
    Given I navigate to "https://example.com"
    Then "h1" should be visible
    And "h1" should not be hidden
    And "body" should be visible
    And "h1" should have text "Example Domain"
    And "p" should contain text "for use in"
    And there should be 1 "h1" element
    And there should be 2 "p" elements
