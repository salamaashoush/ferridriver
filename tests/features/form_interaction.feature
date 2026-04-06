Feature: Form Interaction
  Complex form interactions on a real form page.

  Background:
    Given I navigate to "https://demo.playwright.dev/todomvc/#/"

  Scenario: Type text character by character
    When I type "Hello World" into ".new-todo"
    And I press "Enter"
    Then ".todo-list li" should contain text "Hello World"

  Scenario: Clear and re-fill
    When I fill ".new-todo" with "Original text"
    And I clear ".new-todo"
    And I fill ".new-todo" with "Replaced text"
    And I press "Enter"
    Then ".todo-list li" should contain text "Replaced text"

  Scenario: Multiple rapid inputs
    When I fill ".new-todo" with "Item A"
    And I press "Enter"
    And I fill ".new-todo" with "Item B"
    And I press "Enter"
    And I fill ".new-todo" with "Item C"
    And I press "Enter"
    And I fill ".new-todo" with "Item D"
    And I press "Enter"
    And I fill ".new-todo" with "Item E"
    And I press "Enter"
    Then there should be 5 ".todo-list li" elements
