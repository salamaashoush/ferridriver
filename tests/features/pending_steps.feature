Feature: Pending Steps
  Undefined steps are treated as pending in non-strict mode.

  Scenario: Undefined step becomes pending
    Given I navigate to "https://example.com"
    When I do something that is not yet implemented
    Then the page title should contain "Example"

  Scenario: Multiple undefined steps
    Given I set up the test environment
    When I perform the unimplemented action
    Then the results should be verified
