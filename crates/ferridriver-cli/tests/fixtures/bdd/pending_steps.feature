Feature: Pending Steps
  Undefined steps fail the run by default and are reported as pending under --no-strict.

  Scenario: Undefined step becomes pending
    Given a blank page
    When I do something that is not yet implemented
    Then the page is still blank

  Scenario: Multiple undefined steps
    Given I set up the test environment
    When I perform the unimplemented action
    Then the results should be verified
