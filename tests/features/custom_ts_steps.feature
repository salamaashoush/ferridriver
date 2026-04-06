Feature: Custom TypeScript Steps
  Verify that custom TS step definitions work alongside built-in Rust steps.

  Scenario: Use custom TS step
    Given I am on a blank page
    Then the URL should contain "about:blank"
