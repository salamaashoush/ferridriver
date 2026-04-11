Feature: Screenshots
  Taking screenshots in various formats and scopes.

  Scenario: Take viewport screenshot of colorful grid
    Given I navigate to "/grid.html"
    Then I should be able to take a screenshot

  Scenario: Take full page screenshot of long scrollable content
    Given I navigate to "/screenshots/grid-fullpage.html"
    Then I should be able to take a full page screenshot

  Scenario: Take element screenshot
    Given I navigate to "/input/button.html"
    Then I should be able to take a screenshot of "button"
