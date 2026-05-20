Feature: expect() in TypeScript step bodies

  Scenario: value matchers and asymmetric matchers
    Given a fresh page is loaded
    Then the synthetic JSON matches the expected shape
    And the synthetic JSON satisfies asymmetric matchers
    And toThrow captures a throwing closure

  Scenario: web-first matchers via expect()
    Given the page is navigated to a fixture
    Then the heading element is visible
    And the heading has the expected text
    And the page has the expected title

  Scenario: classic function body still receives World as first arg
    Given the page is set up via a classic function step
    Then the heading element is visible
