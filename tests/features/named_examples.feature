Feature: Named Examples

  Scenario Outline: Visit a website
    Given I navigate to "<url>"
    Then the page title should contain "<title>"

    Examples: Popular sites
      | url                    | title   |
      | https://example.com    | Example |
      | https://www.google.com | Google  |

    Examples: Example domain
      | url                 | title   |
      | https://example.com | Example |
