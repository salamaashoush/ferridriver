Feature: Assertions
  Element state and content assertions.

  Scenario: Text content assertions
    Given I navigate to "http://127.0.0.1:47831/example-domain.html"
    Then "h1" should have text "Example Domain"
    And "p" should contain text "for use in"

  Scenario: Element visibility
    Given I navigate to "http://127.0.0.1:47831/example-domain.html"
    Then "h1" should be visible
    And "body" should be visible

  Scenario: Element count
    Given I navigate to "http://127.0.0.1:47831/example-domain.html"
    Then there should be 1 "h1" element
    And there should be 2 "p" elements

  Scenario: Page title
    Given I navigate to "http://127.0.0.1:47831/example-domain.html"
    Then the page title should be "Example Domain"
    And the page title should contain "Example"
