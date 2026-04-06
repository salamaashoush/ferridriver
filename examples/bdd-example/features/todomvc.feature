Feature: TodoMVC
  Test the Playwright TodoMVC demo with custom and built-in steps.

  Background:
    Given I navigate to "https://demo.playwright.dev/todomvc/#/"

  Scenario: Add and complete a todo
    When I fill ".new-todo" with "Write BDD tests"
    And I press "Enter"
    Then ".todo-list li" should be visible
    And ".todo-list li" should contain text "Write BDD tests"
    When I click ".todo-list li .toggle"
    Then ".todo-list li" should have class "completed"

  @skip
  Scenario: Skipped scenario
    When I fill ".new-todo" with "This is skipped"
    And I press "Enter"
