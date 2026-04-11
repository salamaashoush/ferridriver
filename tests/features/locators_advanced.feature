Feature: Locators advanced
  Advanced locator strategies with the locators test fixture.

  Scenario: Find element by role
    Given I navigate to "/locators.html"
    Then "button[type='submit']" should have text "Submit"

  Scenario: Find element by label
    Given I navigate to "/locators.html"
    Then "#username" should be visible

  Scenario: Find element by placeholder
    Given I navigate to "/locators.html"
    Then "[placeholder='Enter username']" should be visible

  Scenario: Count list items
    Given I navigate to "/locators.html"
    Then there should be 5 "#item-list li" elements

  Scenario: Hidden element is not visible
    Given I navigate to "/locators.html"
    Then "#hidden-section" should not be visible

  Scenario: Fill by label and verify
    Given I navigate to "/locators.html"
    When I fill "#username" with "testuser"
    And I fill "#email" with "test@example.com"
    Then "#username" should have value "testuser"
    And "#email" should have value "test@example.com"

  Scenario: Disabled button is not clickable
    Given I navigate to "/locators.html"
    Then "button[disabled]" should be visible
    And "button[disabled]" should have text "Disabled"

  Scenario: Navigate links
    Given I navigate to "/locators.html"
    Then "nav a" should be visible
    And there should be 3 "nav a" elements
