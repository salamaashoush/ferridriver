@dialog
Feature: Dialogs
  Dialog handling: accept, dismiss, and type into browser dialogs.

  Scenario: Accept an alert dialog
    Given I navigate to "data:text/html,<button onclick=\"alert('Hello!')\">Show Alert</button>"
    And I accept the dialog
    When I click "button"
    Then I should see dialog with text "Hello!"
    And I should have seen 1 dialog

  Scenario: Dismiss a confirm dialog
    Given I navigate to "data:text/html,<button onclick=\"document.title=confirm('Sure?')?'yes':'no'\">Confirm</button>"
    And I dismiss the dialog
    When I click "button"
    Then I should see dialog with text "Sure?"
    And the page title should be "no"

  Scenario: Type into a prompt dialog
    Given I navigate to "data:text/html,<button onclick=\"document.title=prompt('Name?')\">Prompt</button>"
    And I type "ferridriver" in the dialog
    When I click "button"
    Then I should see dialog with text "Name?"
    And the page title should be "ferridriver"
