Feature: Scenario Outline
  Parameterized scenarios using Examples tables.

  Scenario Outline: Navigate to different sites
    Given I navigate to "<url>"
    Then the page title should contain "<expected_title>"

    Examples:
      | url                                        | expected_title |
      | http://127.0.0.1:47831/example-domain.html | Example        |
      | https://www.google.com                     | Google         |

  Scenario Outline: Check element visibility on different pages
    Given I navigate to "<url>"
    Then "<selector>" should be visible

    Examples:
      | url                                        | selector |
      | http://127.0.0.1:47831/example-domain.html | h1       |
      | http://127.0.0.1:47831/example-domain.html | p        |
      | http://127.0.0.1:47831/example-domain.html | body     |
