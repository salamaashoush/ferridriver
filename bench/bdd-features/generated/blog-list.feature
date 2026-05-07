Feature: Blog — list and search

  Scenario Outline: blog search by tag <tag> #<i>
    Given I navigate to "/blog"
    Then "[data-testid=blog-list] li" should be visible
    When I fill "[data-testid=blog-search]" with "<tag>"
    Then "[data-testid=blog-count]" should contain text "matches"
    Then "[data-testid=blog-list] li" should be visible

    Examples:
      | i | tag        |
      | 0 | rust |
      | 1 | typescript |
      | 2 | react |
      | 3 | cdp |
      | 4 | perf |
      | 5 | web |
      | 6 | ai |
      | 7 | api |
      | 8 | rust |
      | 9 | typescript |
      | 10 | react |
      | 11 | cdp |
      | 12 | perf |
      | 13 | web |
      | 14 | ai |
      | 15 | api |
      | 16 | rust |
      | 17 | typescript |
      | 18 | react |
      | 19 | cdp |
      | 20 | perf |
      | 21 | web |
      | 22 | ai |
      | 23 | api |
      | 24 | rust |
      | 25 | typescript |
      | 26 | react |
      | 27 | cdp |
      | 28 | perf |
      | 29 | web |
      | 30 | ai |
      | 31 | api |
      | 32 | rust |
      | 33 | typescript |
      | 34 | react |
      | 35 | cdp |
      | 36 | perf |
      | 37 | web |
      | 38 | ai |
      | 39 | api |
      | 40 | rust |
      | 41 | typescript |
      | 42 | react |
      | 43 | cdp |
      | 44 | perf |
      | 45 | web |
      | 46 | ai |
      | 47 | api |
      | 48 | rust |
      | 49 | typescript |
