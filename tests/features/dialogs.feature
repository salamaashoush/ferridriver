@dialog
Feature: Dialogs
  Dialog handling: accept, dismiss, and type into browser dialogs.

  Scenario: Accept an alert dialog
    Given I navigate to "https://example.com"
    And I accept the dialog
    When I evaluate "alert('Hello!')"
    Then I should see dialog with text "Hello!"

  Scenario: Dismiss a confirm dialog
    Given I navigate to "https://example.com"
    And I dismiss the dialog
    When I evaluate "document.title = String(confirm('Sure?'))"
    Then I should see dialog with text "Sure?"
    And the page title should be "false"

  Scenario: Type into a prompt dialog
    Given I navigate to "https://example.com"
    And I type "ferridriver" in the dialog
    When I evaluate "document.title = prompt('Name?')"
    Then I should see dialog with text "Name?"
    And the page title should be "ferridriver"
