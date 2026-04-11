Feature: Keyboard interactions
  Typing, key combos, and keyboard events.

  Scenario: Type text into textarea
    Given I navigate to "/input/textarea.html"
    When I fill "textarea" with "Hello World"
    Then "textarea" should have value "Hello World"

  Scenario: Press Enter key inserts newline
    Given I navigate to "/input/textarea.html"
    When I click "textarea"
    And I type "Line1"
    And I press "Enter"
    And I type "Line2"
    Then I evaluate "document.querySelector('textarea').value.includes('Line1')" and expect "true"
    And I evaluate "document.querySelector('textarea').value.includes('Line2')" and expect "true"

  Scenario: Type into input field
    Given I navigate to "/input/textarea.html"
    When I click "#input"
    And I type "typed text"
    Then I evaluate "window.result" and expect "typed text"
