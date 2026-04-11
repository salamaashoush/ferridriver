Feature: Forms advanced
  Complex form interactions with the registration form fixture.

  Scenario: Fill registration form and submit
    Given I navigate to "/forms.html"
    When I fill "#fullname" with "Jane Smith"
    And I fill "#email" with "jane@example.com"
    And I fill "#password" with "securepass123"
    And I fill "#confirm-password" with "securepass123"
    And I select "United Kingdom" from "#country"
    And I check "#terms"
    And I click "#submit-btn"
    Then I evaluate "window.submitted" and expect "true"

  Scenario: Form validation prevents empty submit
    Given I navigate to "/forms.html"
    When I click "#submit-btn"
    Then I evaluate "window.submitted" and expect "false"

  Scenario: Checkbox toggle
    Given I navigate to "/input/checkbox.html"
    When I check "#agree"
    Then "#agree" should be checked
    When I uncheck "#agree"
    Then "#agree" should not be checked

  Scenario: Select option by value
    Given I navigate to "/input/select.html"
    When I select "Blue" from "#single"
    Then I evaluate "window.singleResult" and expect "blue"

  Scenario: Fill multiple form fields
    Given I navigate to "/forms.html"
    When I fill "#fullname" with "John Doe"
    And I fill "#email" with "john@test.com"
    And I fill "#phone" with "+1 555 123 4567"
    And I fill "#bio" with "A software engineer from London."
    Then "#fullname" should have value "John Doe"
    And "#email" should have value "john@test.com"
    And "#phone" should have value "+1 555 123 4567"
