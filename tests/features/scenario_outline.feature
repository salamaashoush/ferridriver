Feature: Scenario Outline
  Parameterized scenarios using Examples tables.

  Scenario Outline: Navigate to different sites
    Given I navigate to "<url>"
    Then the page title should contain "<expected_title>"

    Examples:
      | url                     | expected_title |
      | https://example.com     | Example        |
      | https://www.google.com  | Google         |

  Scenario Outline: Check element visibility on different pages
    Given I navigate to "<url>"
    Then "<selector>" should be visible

    Examples:
      | url                 | selector |
      | https://example.com | h1       |
      | https://example.com | p        |
      | https://example.com | body     |
