Feature: Ambiguous steps
  Two step definitions matching one step line is a definition bug, not a pending step.

  Scenario: Two definitions match
    Given a blank page
    When I trigger the ambiguous step
