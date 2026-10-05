Feature: Named Examples

  Scenario Outline: Visit a website
    Given I navigate to "<url>"
    Then the page title should contain "<title>"

    Examples: Popular sites
      | url                                        | title   |
      | http://127.0.0.1:47831/example-domain.html | Example |
      | https://www.google.com                     | Google  |

    Examples: Example domain
      | url                                        | title   |
      | http://127.0.0.1:47831/example-domain.html | Example |
