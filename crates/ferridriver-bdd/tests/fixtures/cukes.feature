Feature: Cukes

  Pure-logic scenarios: a data table, a scenario outline, a deliberate
  failure, and a tag-excluded scenario.

  @smoke
  Scenario: eat some cukes
    Given I have 5 cukes in my belly
    When I eat 3 cukes
    Then I have 2 cukes left

  @smoke
  Scenario: data table sum
    Then the data table sums to 6
      | amount |
      | 1      |
      | 2      |
      | 3      |

  @smoke
  Scenario Outline: outline math
    Given I have <start> cukes in my belly
    When I eat <eat> cukes
    Then I have <left> cukes left

    Examples:
      | start | eat | left |
      | 10    | 4   | 6    |
      | 7     | 7   | 0    |

  @smoke
  Scenario: deliberately failing
    Given I have 5 cukes in my belly
    Then this step always fails

  @wip
  Scenario: excluded by tag filter
    Then this step always fails
