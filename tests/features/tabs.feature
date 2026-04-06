@tab
Feature: Tabs
  Multi-tab workflows: open, switch, and close tabs.

  Scenario: Open a new tab and switch back
    Given I navigate to "data:text/html,<h1>Original Tab</h1>"
    When I open a new tab
    Then I should see 2 tabs
    When I switch to tab 0
    Then "h1" should contain text "Original Tab"

  Scenario: Close tab and return to original
    Given I navigate to "data:text/html,<h1>First</h1>"
    When I open a new tab
    And I close the current tab
    Then I should see 1 tab
    And "h1" should contain text "First"
