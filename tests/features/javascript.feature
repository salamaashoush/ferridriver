@javascript
Feature: JavaScript
  JavaScript evaluation and script injection.

  Scenario: Execute JavaScript and verify result
    Given I navigate to "data:text/html,<h1 id=\"target\">Before</h1>"
    When I evaluate "document.getElementById('target').textContent = 'After'"
    Then "#target" should have text "After"

  Scenario: Store JavaScript result in a variable
    Given I navigate to "data:text/html,<p id=\"count\">42</p>"
    When I store the result of "document.getElementById('count').textContent" as "myCount"
    And I evaluate "document.title = '42'"
    Then the page title should be "42"

  Scenario: Inject a script tag that modifies the DOM
    Given I navigate to "data:text/html,<div id=\"output\">empty</div>"
    When I evaluate "var s=document.createElement('script');s.textContent='document.getElementById(\"output\").textContent=\"injected\"';document.head.appendChild(s)"
    Then "#output" should have text "injected"
