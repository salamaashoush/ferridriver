Feature: Calc

  @smoke
  Scenario: add up
    Given I start with 10
    When I add 5
    Then the total is 15
