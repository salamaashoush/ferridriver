Feature: Custom parameter types
  A registered parameter type keeps its own regex semantics inside a cucumber expression.

  Scenario: Alternation
    Given a blank page
    When I pick green paint
    Then the paint order is "GREEN"

  Scenario: Metacharacters
    Given a blank page
    When I pay 42 coins
    Then the payment is "42"

  Scenario: Alongside built-in parameter types
    Given a blank page
    When I order 3 cans of blue paint
    Then the paint order is "BLUE"
    And the can count is 3
