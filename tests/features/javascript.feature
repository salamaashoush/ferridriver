@javascript
Feature: JavaScript
  JavaScript evaluation and script injection.

  Scenario: Execute JavaScript to modify DOM
    Given I navigate to "https://example.com"
    When I evaluate "document.querySelector('h1').textContent = 'Modified'"
    Then "h1" should have text "Modified"

  Scenario: Store JavaScript result in a variable
    Given I navigate to "https://example.com"
    When I store the result of "document.querySelectorAll('p').length.toString()" as "count"
    And I evaluate "document.title = document.querySelectorAll('p').length.toString()"
    Then the page title should contain "2"

  Scenario: Evaluate JavaScript that sets the title
    Given I navigate to "https://example.com"
    When I evaluate "document.title = 'custom-title'"
    Then the page title should be "custom-title"
