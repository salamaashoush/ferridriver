@cookie
Feature: Cookies
  Cookie management via the browser context API.

  Scenario: Set and verify a cookie
    Given I navigate to "data:text/html,<h1>Cookie Test</h1>"
    When I set cookie "test_name" to "test_value"
    And I evaluate "document.title = document.cookie"
    Then the page title should contain "test_name=test_value"

  Scenario: Delete a specific cookie
    Given I navigate to "data:text/html,<h1>Cookie Test</h1>"
    When I set cookie "to_delete" to "gone"
    And I delete cookie "to_delete"
    And I evaluate "document.title = document.cookie || 'empty'"
    Then the page title should be "empty"

  Scenario: Clear all cookies
    Given I navigate to "data:text/html,<h1>Cookie Test</h1>"
    When I set cookie "a" to "1"
    And I set cookie "b" to "2"
    And I clear all cookies
    And I evaluate "document.title = document.cookie || 'empty'"
    Then the page title should be "empty"
