Feature: Wait and Keyboard
  Wait conditions and keyboard interactions.

  Scenario: Wait for element
    Given I navigate to "https://demo.playwright.dev/todomvc/#/"
    When I fill ".new-todo" with "Wait for me"
    And I press "Enter"
    And I wait for ".todo-list li"
    Then ".todo-list li" should be visible

  Scenario: Keyboard navigation
    Given I navigate to "https://demo.playwright.dev/todomvc/#/"
    When I fill ".new-todo" with "Tab test"
    And I press "Enter"
    And I press "Tab"
    Then ".new-todo" should be visible

  Scenario: Wait with timeout
    Given I navigate to "https://example.com"
    When I wait 500 milliseconds
    Then "h1" should be visible
