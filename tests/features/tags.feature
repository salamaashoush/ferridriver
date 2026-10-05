@smoke
Feature: Tag Filtering
  Tests for tag-based scenario filtering.

  @fast
  Scenario: Fast smoke test
    Given I navigate to "http://127.0.0.1:47831/example-domain.html"
    Then "h1" should be visible

  @slow
  Scenario: Slow test with wait
    Given I navigate to "http://127.0.0.1:47831/example-domain.html"
    When I wait 1 seconds
    Then the page title should contain "Example"

  @skip
  Scenario: Skipped scenario
    Given I navigate to "https://nonexistent.example.com"
    Then "h1" should be visible
