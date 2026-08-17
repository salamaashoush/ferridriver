@serial
Feature: Fixture graph on the BDD world
  A scenario resolves the fixtures its steps destructure, through the
  same graph a Playwright spec resolves through: dependency order up,
  LIFO teardown down, auto fixtures without being named, and worker
  scope shared across the scenarios one worker runs.

  Scenario: A step destructures a fixture of the chain it was bound to
    Given a fixture-backed scenario
    Then the greeting fixture reads "hello from a fixture"
    And the world carries the browser bindings

  Scenario: An auto fixture runs for every scenario without being named
    Given a fixture-backed scenario
    Then no step named the auto fixture and it still ran
    And the auto fixture has run 2 times

  Scenario: Nested fixtures set up in dependency order
    Given a fixture-backed scenario
    When I use the nested fixtures
    Then the nested fixture value is "outer/inner"

  Scenario: The previous scenario's fixtures tore down after its last step
    Given a fixture-backed scenario
    Then the previous scenario tore its fixtures down in LIFO order

  Scenario: A worker fixture is set up once for the whole worker
    Given a fixture-backed scenario
    Then the worker fixture reads "w1"
    And the worker fixture was set up 1 time

  Scenario: Worker scope is shared while test scope is per scenario
    Given a fixture-backed scenario
    Then the worker fixture reads "w1"
    And the worker fixture was set up 1 time
    And the greeting fixture reads "hello from a fixture"
    And the test fixture was set up 2 times
