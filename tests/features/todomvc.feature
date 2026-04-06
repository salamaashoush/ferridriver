Feature: TodoMVC
  Test a full TodoMVC application workflow.

  Background:
    Given I navigate to "https://demo.playwright.dev/todomvc/#/"

  Scenario: Add a todo item
    When I fill ".new-todo" with "Buy groceries"
    And I press "Enter"
    Then ".todo-list li" should be visible
    And ".todo-list li" should contain text "Buy groceries"

  Scenario: Add multiple todo items
    When I fill ".new-todo" with "Item 1"
    And I press "Enter"
    And I fill ".new-todo" with "Item 2"
    And I press "Enter"
    And I fill ".new-todo" with "Item 3"
    And I press "Enter"
    Then there should be 3 ".todo-list li" elements

  Scenario: Complete a todo item
    When I fill ".new-todo" with "Complete me"
    And I press "Enter"
    And I click ".todo-list li .toggle"
    Then ".todo-list li" should have class "completed"

  Scenario: Delete a todo item
    When I fill ".new-todo" with "Delete me"
    And I press "Enter"
    And I hover over ".todo-list li"
    And I click ".todo-list li .destroy"
    Then there should be 0 ".todo-list li" elements

  Scenario: Filter active todos
    When I fill ".new-todo" with "Active item"
    And I press "Enter"
    And I fill ".new-todo" with "Completed item"
    And I press "Enter"
    And I click ".todo-list li:last-child .toggle"
    And I click "a[href='#/active']"
    Then there should be 1 ".todo-list li" elements
    And ".todo-list li" should contain text "Active item"

  Scenario: Filter completed todos
    When I fill ".new-todo" with "Active item"
    And I press "Enter"
    And I fill ".new-todo" with "Completed item"
    And I press "Enter"
    And I click ".todo-list li:last-child .toggle"
    And I click "a[href='#/completed']"
    Then there should be 1 ".todo-list li" elements
    And ".todo-list li" should contain text "Completed item"

  Scenario: Clear completed todos
    When I fill ".new-todo" with "Item to complete"
    And I press "Enter"
    And I click ".todo-list li .toggle"
    And I click ".clear-completed"
    Then there should be 0 ".todo-list li" elements

  Scenario: Edit a todo item
    When I fill ".new-todo" with "Edit me"
    And I press "Enter"
    And I double click ".todo-list li label"
    And I press "Control+a" on ".todo-list li .edit"
    And I type "Edited item" into ".todo-list li .edit"
    And I press "Enter"
    Then ".todo-list li" should contain text "Edited item"
