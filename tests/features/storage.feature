@storage
Feature: Storage
  LocalStorage and SessionStorage operations.

  Scenario: Set and get localStorage value
    Given I navigate to "data:text/html,<h1>Storage</h1>"
    When I set local storage "color" to "blue"
    And I evaluate "document.title = localStorage.getItem('color')"
    Then the page title should be "blue"

  Scenario: Clear localStorage
    Given I navigate to "data:text/html,<h1>Storage</h1>"
    When I set local storage "key1" to "val1"
    And I set local storage "key2" to "val2"
    And I clear local storage
    And I evaluate "document.title = localStorage.length.toString()"
    Then the page title should be "0"

  Scenario: Set and get sessionStorage value
    Given I navigate to "data:text/html,<h1>Storage</h1>"
    When I set session storage "token" to "abc123"
    And I evaluate "document.title = sessionStorage.getItem('token')"
    Then the page title should be "abc123"

  Scenario: Clear sessionStorage
    Given I navigate to "data:text/html,<h1>Storage</h1>"
    When I set session storage "a" to "1"
    And I set session storage "b" to "2"
    And I clear session storage
    And I evaluate "document.title = sessionStorage.length.toString()"
    Then the page title should be "0"
