Feature: Saved task workflows
  Task changes should survive reopening the app without changing unrelated tasks.

  Scenario: Complete, filter and reopen a task list
    Given my task list contains:
      | Review gate results |
      | Add browser coverage |
    When I click ".todo-list li:first-child .toggle"
    And I reload the page
    Then the task list shows in order:
      | Review gate results |
      | Add browser coverage |
    And ".todo-list li:first-child" should have class "completed"
    And the task list has 1 remaining tasks
    When I click "a[href='#/active']"
    Then the task list shows in order:
      | Add browser coverage |
    When I click "a[href='#/completed']"
    Then the task list shows in order:
      | Review gate results |
    When I click ".clear-completed"
    And I click "a[href='#/']"
    And I reload the page
    Then the task list shows in order:
      | Add browser coverage |
    And the task list has 1 remaining tasks

  Scenario: Save an edit and cancel a later edit without losing the saved title
    Given my task list contains:
      | Review gate results |
      | Add browser coverage |
    When I double click ".todo-list li:first-child label"
    And I fill ".todo-list li:first-child .edit" with "Review Firefox results"
    And I press "Enter" on ".todo-list li:first-child .edit"
    And I reload the page
    Then the task list shows in order:
      | Review Firefox results |
      | Add browser coverage   |
    When I double click ".todo-list li:first-child label"
    And I fill ".todo-list li:first-child .edit" with "Discard this draft"
    And I press "Escape" on ".todo-list li:first-child .edit"
    And I reload the page
    Then the task list shows in order:
      | Review Firefox results |
      | Add browser coverage   |
    And the task list has 2 remaining tasks

  Scenario: Completing one duplicate title does not complete or delete the other
    Given my task list contains:
      | Review gate results |
      | Review gate results |
    When I click ".todo-list li:first-child .toggle"
    And I click ".clear-completed"
    And I reload the page
    Then the task list shows in order:
      | Review gate results |
    And the task list has 1 remaining tasks
    And ".todo-list li .toggle" should not be checked
