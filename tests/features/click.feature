Feature: Click interactions
  Advanced click scenarios using test fixtures.

  Scenario: Click button and verify result
    Given I navigate to "/input/button.html"
    When I click "button"
    Then I evaluate "window.result" and expect "Clicked"

  Scenario: Double click button
    Given I navigate to "/input/button.html"
    When I double click "button"
    Then I evaluate "window.result" and expect "Double-clicked"

  Scenario: Click button in scrollable page
    Given I navigate to "/input/scrollable.html"
    When I click "#button-50"
    Then I evaluate "document.getElementById('button-50').textContent" and expect "clicked"

  Scenario: Click offscreen button scrolls into view
    Given I navigate to "/input/scrollable.html"
    When I click "#button-99"
    Then I evaluate "document.getElementById('button-99').textContent" and expect "clicked"
