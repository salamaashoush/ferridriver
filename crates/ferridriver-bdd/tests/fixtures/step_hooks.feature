Feature: Step hooks

  Scenario: hooks fire around every executed step
    Given a counted step
    Given a counted step
    Given a failing counted step
    Given a counted step

  Scenario: counters observed
    Given hook counters are 4 and 3 with failure seen true
